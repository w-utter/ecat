use crate::setup::setup_write;
use crate::txbuf::TxBuf;
use ethercrab::{
    ConfigureDevices, DeviceProperties, EtherCrabWireSized, MainDevice, PduHeader,
    PrepConfigureDevices, PrepDeviceProperties, error::Error, received_frame::ReceivedPdu,
    std::RawSocketDesc,
};
use io_uring::{IoUring, types::Timespec};
use std::collections::BTreeMap;

use crate::eeprom::{category::CategoryReader, range::RangeReader, string::StringReader};

use heapless::Deque;

pub struct Init<const N: usize> {
    subdevices: Deque<SubdevState, N>,
    state: InitState,
}

impl<const N: usize> Init<N> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_new(
        subdev_count: u16,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        write_entry: impl Fn(u64) -> u64,
        timeout_entry: impl Fn(u64) -> u64,
    ) -> Result<Self, Error> {
        let mut subdevices = Deque::new();

        let mut addr_state = PrepConfigureDevices::new(subdev_count);

        let (res, id, addr) = ConfigureDevices::iter(maindevice, &mut addr_state).unwrap();
        let (frame, handle) = res?.unwrap();

        setup_write(
            frame,
            handle,
            retry_count,
            timeout,
            tx_entries,
            sock,
            ring,
            Some(id),
            None,
            write_entry,
            timeout_entry,
        )?;

        let _ = subdevices.push_back(SubdevState::new(addr));
        let state = InitState::ConfigureAddresses(addr_state);

        Ok(Self { subdevices, state })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn update(
        &mut self,
        received: ReceivedPdu<'_>,
        header: PduHeader,
        maindevice: &mut MainDevice,
        retry_count: usize,
        timeout: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut io_uring::IoUring,
        idx: Option<u16>,
        identifier: Option<u8>,
        write_entry: impl Fn(u64) -> u64,
        timeout_entry: impl Fn(u64) -> u64,
    ) -> Result<Option<Deque<ethercrab::SubDevice, N>>, Error> {
        match &mut self.state {
            InitState::ConfigureAddresses(addr_state) => {
                if header.command_code != 2 {
                    unreachable!()
                }

                if let Some((res, id, addr)) = ConfigureDevices::iter(maindevice, addr_state) {
                    let (frame, handle) = res?.unwrap();

                    setup_write(
                        frame,
                        handle,
                        retry_count,
                        timeout,
                        tx_entries,
                        sock,
                        ring,
                        Some(id),
                        None,
                        &write_entry,
                        &timeout_entry,
                    )?;

                    let _ = self.subdevices.push_back(SubdevState::new(addr));

                    return Ok(None);
                }

                let (frame, handle) = maindevice
                    .prep_wait_for_state(ethercrab::SubDeviceState::Init)?
                    .unwrap();
                setup_write(
                    frame,
                    handle,
                    retry_count,
                    timeout,
                    tx_entries,
                    sock,
                    ring,
                    None,
                    None,
                    &write_entry,
                    &timeout_entry,
                )?;

                self.state = InitState::SyncInit;
            }
            InitState::SyncInit => {
                if header.command_code != 7 {
                    unreachable!()
                }

                use ethercrab::EtherCrabWireRead;
                let ctrl = ethercrab::AlControl::unpack_from_slice(&received);

                if !matches!(
                    ctrl.map(|ctrl| ctrl.state),
                    Ok(ethercrab::SubDeviceState::Init)
                ) {
                    todo!("handle devices that are not in init state during sync");
                }

                let subdev = self.subdevices.front_mut().unwrap();

                subdev.start(
                    maindevice,
                    retry_count,
                    timeout,
                    tx_entries,
                    sock,
                    ring,
                    0,
                    &write_entry,
                    &timeout_entry,
                )?;

                self.state = InitState::ConfigureSubdevices(0);
            }
            InitState::ConfigureSubdevices(configured_idx) => {
                let id = idx.expect("no index set :(");

                let subdev = self
                    .subdevices
                    .get_mut(usize::from(id))
                    .expect("could not get subdev");

                if !subdev.update(
                    received,
                    header,
                    maindevice,
                    retry_count,
                    timeout,
                    tx_entries,
                    sock,
                    ring,
                    id,
                    identifier,
                    &write_entry,
                    &timeout_entry,
                )? {
                    return Ok(None);
                }

                *configured_idx += 1;

                if usize::from(*configured_idx) != self.subdevices.len() {
                    let subdev = self
                        .subdevices
                        .get_mut(usize::from(*configured_idx))
                        .unwrap();

                    subdev.start(
                        maindevice,
                        retry_count,
                        timeout,
                        tx_entries,
                        sock,
                        ring,
                        *configured_idx,
                        &write_entry,
                        &timeout_entry,
                    )?;
                    return Ok(None);
                }

                let mut subdevs = heapless::Deque::new();
                let devs = core::mem::take(&mut self.subdevices);

                for subdev in devs.into_iter() {
                    match subdev {
                        SubdevState::Init(dev) => {
                            let _ = subdevs.push_back(dev);
                        }
                        _ => unreachable!(),
                    }
                }
                return Ok(Some(subdevs));
            }
        }
        Ok(None)
    }
}

enum InitState {
    ConfigureAddresses(PrepConfigureDevices),
    SyncInit,
    ConfigureSubdevices(u16),
}

enum SubdevState {
    Initializing {
        configured_addr: u16,
        state: SubdevInitState,
    },
    Init(ethercrab::SubDevice),
}

impl SubdevState {
    fn new(configured_addr: u16) -> Self {
        Self::Initializing {
            configured_addr,
            state: SubdevInitState::ClearEeprom,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut io_uring::IoUring,
        idx: u16,
        write_entry: impl Fn(u64) -> u64,
        timeout_entry: impl Fn(u64) -> u64,
    ) -> Result<(), Error> {
        let configured_addr = match self {
            Self::Initializing {
                configured_addr, ..
            } => configured_addr,
            _ => unreachable!(),
        };
        let (frame, handle) = maindevice
            .prep_clear_eeprom(*configured_addr)
            .unwrap()
            .unwrap();

        setup_write(
            frame,
            handle,
            retry_count,
            timeout,
            tx_entries,
            sock,
            ring,
            Some(idx),
            None,
            write_entry,
            timeout_entry,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn update(
        &mut self,
        received: ReceivedPdu<'_>,
        header: PduHeader,
        maindevice: &mut MainDevice,
        retry_count: usize,
        timeout: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut io_uring::IoUring,
        idx: u16,
        identifier: Option<u8>,
        write_entry: impl Fn(u64) -> u64,
        timeout_entry: impl Fn(u64) -> u64,
    ) -> Result<bool, Error> {
        match self {
            Self::Initializing {
                configured_addr,
                state,
            } => match state {
                SubdevInitState::ClearEeprom => {
                    if header.command_code != 5 {
                        unreachable!()
                    }

                    let (frame, handle) = maindevice
                        .prep_set_eeprom(*configured_addr, ethercrab::SiiOwner::Master)?
                        .unwrap();

                    setup_write(
                        frame,
                        handle,
                        retry_count,
                        timeout,
                        tx_entries,
                        sock,
                        ring,
                        Some(idx),
                        None,
                        write_entry,
                        timeout_entry,
                    )?;

                    *state = SubdevInitState::SetEeprom;
                }
                SubdevInitState::SetEeprom => {
                    if header.command_code != 5 {
                        unreachable!()
                    }

                    let mut prep_state = PrepDeviceProperties::new(*configured_addr);

                    let (frame, handle) = DeviceProperties::iter(maindevice, &mut prep_state)
                        .unwrap()
                        .unwrap()
                        .unwrap();

                    setup_write(
                        frame,
                        handle,
                        retry_count,
                        timeout,
                        tx_entries,
                        sock,
                        ring,
                        Some(idx),
                        None,
                        write_entry,
                        timeout_entry,
                    )?;

                    let identity_state = RangeReader::new(
                        0x0008,
                        ethercrab::SubDeviceIdentity::PACKED_LEN as u16,
                        Default::default(),
                        1,
                    );

                    let name_state = name::NameState::new(2);

                    *state = SubdevInitState::DeviceProperties {
                        prep_state,
                        identity: None,
                        flags: None,
                        alias_address: None,
                        ports: None,
                        identity_state,
                        name_state,
                        complete_access: false,
                    };
                }
                SubdevInitState::DeviceProperties {
                    prep_state,
                    identity,
                    flags,
                    alias_address,
                    ports,
                    identity_state,
                    name_state,
                    complete_access,
                } => {
                    if !matches!(header.command_code, 4 | 5) {
                        unreachable!("{}", header.command_code)
                    }

                    if let Some(res) = DeviceProperties::iter(maindevice, prep_state) {
                        let (frame, handle) = res.unwrap().unwrap();
                        setup_write(
                            frame,
                            handle,
                            retry_count,
                            timeout,
                            tx_entries,
                            sock,
                            ring,
                            Some(idx),
                            None,
                            &write_entry,
                            &timeout_entry,
                        )?;
                    }

                    // only care about upper 16 bits as thats what stores the register
                    // for fprd commands (which these all are)
                    let addr = (u32::from_le_bytes(header.command_raw) >> 16) as u16;
                    use ethercrab::EtherCrabWireRead;

                    // all taken from definitions in register.rs
                    match addr {
                        // support flags
                        0x0008 => {
                            *flags = Some(
                                ethercrab::SupportFlags::unpack_from_slice(&received).unwrap(),
                            );
                        }
                        // station alias
                        0x0012 => *alias_address = Some(u16::unpack_from_slice(&received).unwrap()),
                        // dl status
                        0x0110 => {
                            let status = ethercrab::DlStatus::unpack_from_slice(&received).unwrap();
                            *ports = Some(ethercrab::Ports::new(
                                status.link_port0,
                                status.link_port3,
                                status.link_port1,
                                status.link_port2,
                            ));

                            identity_state.start(
                                maindevice,
                                retry_count,
                                timeout,
                                tx_entries,
                                sock,
                                ring,
                                *configured_addr,
                                idx,
                                &write_entry,
                                &timeout_entry,
                            )?;
                        }
                        0x0502 | 0x0508 => match identifier {
                            Some(1) => {
                                if identity_state.update(
                                    received,
                                    header,
                                    maindevice,
                                    retry_count,
                                    timeout,
                                    tx_entries,
                                    sock,
                                    ring,
                                    *configured_addr,
                                    idx,
                                    &write_entry,
                                    &timeout_entry,
                                )? {
                                    *identity =
                                        Some(ethercrab::SubDeviceIdentity::unpack_from_slice(
                                            &identity_state.buffer,
                                        )?);
                                    name_state.start(
                                        maindevice,
                                        retry_count,
                                        timeout,
                                        tx_entries,
                                        sock,
                                        ring,
                                        *configured_addr,
                                        idx,
                                        &write_entry,
                                        &timeout_entry,
                                    )?;
                                }
                            }
                            Some(2) => {
                                if name_state.update(
                                    received,
                                    header,
                                    maindevice,
                                    retry_count,
                                    timeout,
                                    tx_entries,
                                    sock,
                                    ring,
                                    *configured_addr,
                                    idx,
                                    complete_access,
                                    &write_entry,
                                    &timeout_entry,
                                )? {
                                    todo!()
                                }
                            }
                            _ => unreachable!(),
                        },
                        reg => unreachable!("{:#x}", reg),
                    }

                    if let (
                        Some(identity),
                        name::NameState::Name(_name),
                        Some(flags),
                        Some(alias),
                        Some(ports),
                    ) = (identity, name_state, flags, alias_address, ports)
                    {
                        let mut subdev = ethercrab::SubDevice::new_from_io_uring(
                            *configured_addr,
                            *alias,
                            idx,
                            *identity,
                            Default::default(),
                            *flags,
                            *ports,
                        );
                        // this will be used later when configuring mailboxes
                        subdev.config.mailbox.complete_access = *complete_access;

                        *self = Self::Init(subdev);
                        return Ok(true);
                    }
                }
            },
            _ => unreachable!(),
        }
        Ok(false)
    }
}

enum SubdevInitState {
    ClearEeprom,
    SetEeprom,
    DeviceProperties {
        prep_state: PrepDeviceProperties,
        identity: Option<ethercrab::SubDeviceIdentity>,
        flags: Option<ethercrab::SupportFlags>,
        alias_address: Option<u16>,
        ports: Option<ethercrab::Ports>,
        identity_state: RangeReader<{ ethercrab::SubDeviceIdentity::PACKED_LEN }>,
        name_state: name::NameState<64>,
        complete_access: bool,
    },
}

mod name {
    use super::*;
    pub(crate) enum NameState<const N: usize> {
        FindingCategory(CategoryReader),
        ReadingCategory(RangeReader<{ ethercrab::SiiGeneral::PACKED_LEN }>),
        FindingStrings(CategoryReader, u8),
        ReadingStrings(StringReader),
        ReadingName(RangeReader<N>),
        Name(Option<heapless::String<N>>),
    }

    impl<const N: usize> NameState<N> {
        pub(crate) fn new(identifier: u8) -> Self {
            Self::FindingCategory(CategoryReader::new(
                ethercrab::CategoryType::General,
                identifier,
            ))
        }

        #[allow(clippy::too_many_arguments)]
        pub(crate) fn start(
            &mut self,
            maindevice: &MainDevice,
            retry_count: usize,
            timeout_duration: &Timespec,
            tx_entries: &mut BTreeMap<u64, TxBuf>,
            sock: &RawSocketDesc,
            ring: &mut io_uring::IoUring,
            configured_addr: u16,
            idx: u16,
            write_entry: impl Fn(u64) -> u64,
            timeout_entry: impl Fn(u64) -> u64,
        ) -> Result<(), Error> {
            match self {
                Self::FindingCategory(state) => state.start(
                    maindevice,
                    retry_count,
                    timeout_duration,
                    tx_entries,
                    sock,
                    ring,
                    configured_addr,
                    idx,
                    write_entry,
                    timeout_entry,
                ),
                _ => unreachable!(),
            }
        }

        #[allow(clippy::too_many_arguments)]
        pub(crate) fn update(
            &mut self,
            received: ReceivedPdu<'_>,
            header: PduHeader,
            maindevice: &mut MainDevice,
            retry_count: usize,
            timeout_duration: &Timespec,
            tx_entries: &mut BTreeMap<u64, TxBuf>,
            sock: &RawSocketDesc,
            ring: &mut io_uring::IoUring,
            configured_addr: u16,
            index: u16,
            complete_access: &mut bool,
            write_entry: impl Fn(u64) -> u64,
            timeout_entry: impl Fn(u64) -> u64,
        ) -> Result<bool, Error> {
            use ethercrab::EtherCrabWireRead;
            match self {
                Self::FindingCategory(cat) => {
                    if cat.update(
                        received,
                        header,
                        maindevice,
                        retry_count,
                        timeout_duration,
                        tx_entries,
                        sock,
                        ring,
                        configured_addr,
                        index,
                        &write_entry,
                        &timeout_entry,
                    )? {
                        match core::mem::take(&mut cat.found) {
                            None => *self = Self::Name(None),
                            Some(found) => {
                                let mut reader = RangeReader::new(
                                    found.start,
                                    found.len,
                                    Default::default(),
                                    cat.identifier,
                                );

                                reader.start(
                                    maindevice,
                                    retry_count,
                                    timeout_duration,
                                    tx_entries,
                                    sock,
                                    ring,
                                    configured_addr,
                                    index,
                                    &write_entry,
                                    &timeout_entry,
                                )?;

                                *self = Self::ReadingCategory(reader)
                            }
                        }
                    }
                }
                Self::ReadingCategory(cat) => {
                    if cat.update(
                        received,
                        header,
                        maindevice,
                        retry_count,
                        timeout_duration,
                        tx_entries,
                        sock,
                        ring,
                        configured_addr,
                        index,
                        &write_entry,
                        &timeout_entry,
                    )? {
                        let general_info =
                            ethercrab::SiiGeneral::unpack_from_slice(&cat.buffer).unwrap();
                        *complete_access = general_info
                            .coe_details
                            .contains(ethercrab::CoeDetails::ENABLE_COMPLETE_ACCESS);
                        let mut reader =
                            CategoryReader::new(ethercrab::CategoryType::Strings, cat.identifier);
                        reader.start(
                            maindevice,
                            retry_count,
                            timeout_duration,
                            tx_entries,
                            sock,
                            ring,
                            configured_addr,
                            index,
                            &write_entry,
                            &timeout_entry,
                        )?;

                        *self = Self::FindingStrings(reader, general_info.name_string_idx);
                    }
                }
                Self::FindingStrings(cat, name_idx) => {
                    if cat.update(
                        received,
                        header,
                        maindevice,
                        retry_count,
                        timeout_duration,
                        tx_entries,
                        sock,
                        ring,
                        configured_addr,
                        index,
                        &write_entry,
                        &timeout_entry,
                    )? {
                        match core::mem::take(&mut cat.found) {
                            None => *self = Self::Name(None),
                            Some(found) => {
                                let mut reader =
                                    StringReader::new(*name_idx, found.start, cat.identifier);
                                reader.start(
                                    maindevice,
                                    retry_count,
                                    timeout_duration,
                                    tx_entries,
                                    sock,
                                    ring,
                                    configured_addr,
                                    index,
                                    &write_entry,
                                    &timeout_entry,
                                )?;

                                *self = Self::ReadingStrings(reader);
                            }
                        }
                    }
                }
                Self::ReadingStrings(strings) => {
                    if strings.update(
                        received,
                        header,
                        maindevice,
                        retry_count,
                        timeout_duration,
                        tx_entries,
                        sock,
                        ring,
                        configured_addr,
                        index,
                        &write_entry,
                        &timeout_entry,
                    )? {
                        match core::mem::take(&mut strings.found) {
                            None => *self = Self::Name(None),
                            Some(found) => {
                                let mut reader = RangeReader::new(
                                    found.start,
                                    found.len,
                                    [0; N],
                                    strings.identifier,
                                );
                                reader.start(
                                    maindevice,
                                    retry_count,
                                    timeout_duration,
                                    tx_entries,
                                    sock,
                                    ring,
                                    configured_addr,
                                    index,
                                    &write_entry,
                                    &timeout_entry,
                                )?;

                                *self = Self::ReadingName(reader);
                            }
                        }
                    }
                }
                Self::ReadingName(name) => {
                    if name.update(
                        received,
                        header,
                        maindevice,
                        retry_count,
                        timeout_duration,
                        tx_entries,
                        sock,
                        ring,
                        configured_addr,
                        index,
                        &write_entry,
                        &timeout_entry,
                    )? {
                        let mut buf = [0; N];
                        core::mem::swap(&mut name.buffer, &mut buf);
                        let mut v = heapless::Vec::from_array(buf);
                        unsafe {
                            v.set_len(name.len as _);
                        }
                        match heapless::String::from_utf8(v) {
                            Ok(str) => *self = Self::Name(Some(str)),
                            Err(_) => *self = Self::Name(None),
                        }
                    }
                }
                _ => todo!(),
            }
            Ok(false)
        }
    }
}
