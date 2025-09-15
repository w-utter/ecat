use crate::setup::setup_write;
use crate::txbuf::TxBuf;
use ethercrab::{
    EtherCrabWireSized, MainDevice, PduHeader, SubDevice, error::Error,
    received_frame::ReceivedPdu, std::RawSocketDesc,
};
use io_uring::{IoUring, types::Timespec};
use std::collections::BTreeMap;

use crate::eeprom::{category::CategoryIter, range::RangeReader};

use crate::state_transition::Transition;

use crate::sdo::SdoRead;

use heapless::Deque;

// configures the mailboxes on all of the ecat slaves to later setup fmmus and sync managers
pub struct MailboxConfig<const N: usize> {
    subdevices: Deque<(SubDevice, MailboxConfigState), N>,
    transition_count: u16,
}

impl<const N: usize> MailboxConfig<N> {
    pub(crate) fn start_new(
        subdevs: Deque<SubDevice, N>,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout_duration: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
    ) -> Result<Self, Error> {
        let mut devs = heapless::Deque::new();
        for (id, subdev) in subdevs.into_iter().enumerate() {
            let mut state = MailboxConfigState::new();
            state.start(
                maindevice,
                retry_count,
                timeout_duration,
                tx_entries,
                sock,
                ring,
                subdev.configured_address(),
                id as u16,
            )?;
            let _ = devs.push_back((subdev, state));
        }

        Ok(Self {
            subdevices: devs,
            transition_count: 0,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn update(
        &mut self,
        received: ReceivedPdu<'_>,
        header: PduHeader,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout_duration: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        identifier: Option<u8>,
        idx: Option<u16>,
    ) -> Result<Option<Deque<(SubDevice, MailboxConfigState), N>>, Error> {
        let idx = idx.unwrap() as usize;
        let (dev, state) = self.subdevices.get_mut(idx).unwrap();

        if state.update(
            received,
            header,
            maindevice,
            retry_count,
            timeout_duration,
            tx_entries,
            sock,
            ring,
            dev.configured_address(),
            idx as u16,
            dev,
            identifier,
        )? {
            self.transition_count += 1;
            if usize::from(self.transition_count) == self.subdevices.len() {
                return Ok(Some(core::mem::take(&mut self.subdevices)));
            }
        }
        Ok(None)
    }
}

pub(crate) enum MailboxConfigState {
    SetEepromMaster,
    SyncManagers(
        CategoryIter<{ ethercrab::SyncManager::PACKED_LEN }>,
        heapless::Vec<ethercrab::SyncManager, 8>,
    ),
    GetMailboxConfig(
        heapless::Vec<ethercrab::SyncManager, 8>,
        RangeReader<{ ethercrab::DefaultMailbox::PACKED_LEN }>,
    ),
    ConfigureMailboxSms(SyncManagerMbxConfig<8>),
    SetEepromPdi,
    PreOpTransition(Transition),
    CoeSyncManagers(SdoRead<heapless::Vec<ethercrab::SyncManagerType, 16>>),
    ResetEepromMaster,
}

impl MailboxConfigState {
    fn new() -> Self {
        Self::SetEepromMaster
    }

    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout_duration: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        configured_addr: u16,
        idx: u16,
    ) -> Result<(), Error> {
        let (frame, handle) = maindevice
            .prep_set_eeprom(configured_addr, ethercrab::SiiOwner::Master)
            .unwrap()
            .unwrap();

        setup_write(
            frame,
            handle,
            retry_count,
            timeout_duration,
            tx_entries,
            sock,
            ring,
            Some(idx),
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn update(
        &mut self,
        received: ReceivedPdu<'_>,
        header: PduHeader,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout_duration: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        configured_addr: u16,
        idx: u16,
        subdev: &mut ethercrab::SubDevice,
        identifier: Option<u8>,
    ) -> Result<bool, Error> {
        match self {
            Self::SetEepromMaster => {
                let mut state = CategoryIter::new(ethercrab::CategoryType::SyncManager, 0);
                state.start(
                    maindevice,
                    retry_count,
                    timeout_duration,
                    tx_entries,
                    sock,
                    ring,
                    configured_addr,
                    idx,
                )?;

                *self = Self::SyncManagers(state, Default::default());
            }
            Self::SyncManagers(managers, collected) => {
                if let Some(more) = managers.update(
                    received,
                    header,
                    maindevice,
                    retry_count,
                    timeout_duration,
                    tx_entries,
                    sock,
                    ring,
                    configured_addr,
                    idx,
                )? {
                    if let Some(buf) = managers.buffer() {
                        use ethercrab::EtherCrabWireRead;
                        let mgr = ethercrab::SyncManager::unpack_from_slice(buf).unwrap();

                        let _ = collected.push(mgr);
                    } else {
                        panic!("could not find sync manager")
                    }

                    if !more {
                        println!("managers: {collected:?}");

                        let mut mbx_config = RangeReader::new(
                            0x0018,
                            ethercrab::DefaultMailbox::PACKED_LEN as _,
                            Default::default(),
                            0,
                        );

                        mbx_config.start(
                            maindevice,
                            retry_count,
                            timeout_duration,
                            tx_entries,
                            sock,
                            ring,
                            configured_addr,
                            idx,
                        )?;

                        println!("\ngetting mbx config\n");
                        *self = Self::GetMailboxConfig(core::mem::take(collected), mbx_config);
                    }
                }
            }
            Self::GetMailboxConfig(sync_managers, config) => {
                if config.update(
                    received,
                    header,
                    maindevice,
                    retry_count,
                    timeout_duration,
                    tx_entries,
                    sock,
                    ring,
                    configured_addr,
                    idx,
                )? {
                    use ethercrab::EtherCrabWireRead;
                    let cfg = ethercrab::DefaultMailbox::unpack_from_slice(&config.buffer)?;

                    let mut mbx_cfg =
                        SyncManagerMbxConfig::new(core::mem::take(sync_managers), cfg);
                    mbx_cfg.update(
                        maindevice,
                        retry_count,
                        timeout_duration,
                        tx_entries,
                        sock,
                        ring,
                        configured_addr,
                        idx,
                    )?;

                    println!("\nconfiguring mailboxes\n");
                    *self = Self::ConfigureMailboxSms(mbx_cfg);
                }
            }
            Self::ConfigureMailboxSms(cfg) => {
                if cfg.update(
                    maindevice,
                    retry_count,
                    timeout_duration,
                    tx_entries,
                    sock,
                    ring,
                    configured_addr,
                    idx,
                )? {
                    let read_mbx = core::mem::take(&mut cfg.read_mbx);
                    let write_mbx = core::mem::take(&mut cfg.write_mbx);

                    subdev.config.mailbox.has_coe = cfg
                        .default_mbx
                        .supported_protocols
                        .contains(ethercrab::MailboxProtocols::COE)
                        && read_mbx.is_some_and(|mbox| mbox.len > 0);

                    subdev.config.mailbox.read = read_mbx;
                    subdev.config.mailbox.write = write_mbx;

                    subdev.config.mailbox.supported_protocols = cfg.default_mbx.supported_protocols;

                    println!("\nsetting eeprom back to pdi\n");

                    let (frame, handle) = maindevice
                        .prep_set_eeprom(configured_addr, ethercrab::SiiOwner::Pdi)
                        .unwrap()
                        .unwrap();

                    setup_write(
                        frame,
                        handle,
                        retry_count,
                        timeout_duration,
                        tx_entries,
                        sock,
                        ring,
                        Some(idx),
                        None,
                    )?;

                    *self = Self::SetEepromPdi;
                }
            }
            Self::SetEepromPdi => {
                let mut state = Transition::new(ethercrab::SubDeviceState::PreOp);
                state.start(
                    maindevice,
                    retry_count,
                    timeout_duration,
                    tx_entries,
                    sock,
                    ring,
                    configured_addr,
                    idx,
                )?;
                *self = Self::PreOpTransition(state);
            }
            Self::PreOpTransition(transition) => {
                if transition.update(
                    received,
                    header,
                    maindevice,
                    retry_count,
                    timeout_duration,
                    tx_entries,
                    sock,
                    ring,
                    configured_addr,
                    idx,
                )? {
                    // onto setting up stuff for coe
                    if !subdev.config.mailbox.complete_access {
                        todo!("support for non complete access devices");
                    }

                    let mbx_count = subdev.mailbox_counter();
                    let mut sdo_read = SdoRead::new(
                        mbx_count,
                        ethercrab::sync_manager_channel::SM_TYPE_ADDRESS,
                        ethercrab::SubIndex::Complete,
                    );

                    println!("reading sync managers from coe");

                    sdo_read.start(
                        maindevice,
                        retry_count,
                        timeout_duration,
                        tx_entries,
                        sock,
                        ring,
                        &subdev.config.mailbox.write.unwrap(),
                        &subdev.config.mailbox.read.unwrap(),
                        configured_addr,
                        identifier,
                        idx,
                    )?;

                    *self = Self::CoeSyncManagers(sdo_read);
                }
            }
            Self::CoeSyncManagers(s) => {
                if let Some(mgrs) = s.update(
                    received,
                    header,
                    maindevice,
                    retry_count,
                    timeout_duration,
                    tx_entries,
                    sock,
                    ring,
                    &subdev.config.mailbox.write.unwrap(),
                    &subdev.config.mailbox.read.unwrap(),
                    configured_addr,
                    identifier,
                    idx,
                )? {
                    subdev.config.mailbox.coe_sync_manager_types = mgrs;

                    println!("resetting eeprom back to master");

                    let (frame, handle) = maindevice
                        .prep_set_eeprom(configured_addr, ethercrab::SiiOwner::Master)
                        .unwrap()
                        .unwrap();

                    setup_write(
                        frame,
                        handle,
                        retry_count,
                        timeout_duration,
                        tx_entries,
                        sock,
                        ring,
                        Some(idx),
                        None,
                    )?;

                    *self = Self::ResetEepromMaster;
                }
            }
            Self::ResetEepromMaster => {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[derive(Debug)]
pub struct SyncManagerMbxConfig<const N: usize> {
    default_mbx: ethercrab::DefaultMailbox,
    read_mbx: Option<ethercrab::Mailbox>,
    write_mbx: Option<ethercrab::Mailbox>,
    iter: std::iter::Enumerate<heapless::vec::IntoIter<ethercrab::SyncManager, N, usize>>,
}

impl<const N: usize> SyncManagerMbxConfig<N> {
    fn new(
        vec: heapless::Vec<ethercrab::SyncManager, N>,
        default_mbx: ethercrab::DefaultMailbox,
    ) -> Self {
        Self {
            default_mbx,
            iter: vec.into_iter().enumerate(),
            read_mbx: None,
            write_mbx: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn update(
        &mut self,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        configured_addr: u16,
        idx: u16,
    ) -> Result<bool, Error> {
        for (sm_idx, sync_manager) in self.iter.by_ref() {
            use ethercrab::SyncManagerType;
            match sync_manager.usage_type() {
                SyncManagerType::MailboxWrite => {
                    println!("mbx write");
                    let ((frame, handle), _) = maindevice
                        .prep_write_sm_config(
                            configured_addr,
                            sm_idx as u8,
                            &sync_manager,
                            self.default_mbx.subdevice_receive_size,
                        )
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
                    )?;

                    self.write_mbx = Some(ethercrab::Mailbox {
                        address: sync_manager.start_addr,
                        len: self.default_mbx.subdevice_receive_size,
                        sync_manager: sm_idx as u8,
                    });
                    return Ok(false);
                }
                SyncManagerType::MailboxRead => {
                    println!("mbx read");
                    let ((frame, handle), _) = maindevice
                        .prep_write_sm_config(
                            configured_addr,
                            sm_idx as u8,
                            &sync_manager,
                            self.default_mbx.subdevice_send_size,
                        )
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
                    )?;

                    self.read_mbx = Some(ethercrab::Mailbox {
                        address: sync_manager.start_addr,
                        len: self.default_mbx.subdevice_send_size,
                        sync_manager: sm_idx as u8,
                    });
                    return Ok(false);
                }
                _ => continue,
            }
        }
        Ok(true)
    }
}
