use crate::setup::setup_write;
use crate::txbuf::TxBuf;
use ethercrab::{
    MainDevice, PduHeader, error::Error, received_frame::ReceivedPdu, std::RawSocketDesc,
};
use io_uring::{IoUring, types::Timespec};
use std::collections::BTreeMap;

#[derive(Debug)]
pub(crate) struct MbxWriteRead<R> {
    req: R,
    state: MbxWriteReadState,
}

impl<R: ethercrab::coe::services::CoeServiceRequest> MbxWriteRead<R> {
    pub(crate) fn new(request: R) -> Self {
        Self {
            req: request,
            state: MbxWriteReadState::MailboxFull(CoeMailboxState::new()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start(
        &mut self,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        write_mbx: &ethercrab::Mailbox,
        read_mbx: &ethercrab::Mailbox,
        configured_addr: u16,
        identifier: Option<u8>,
        idx: u16,
        write_entry: impl Fn(u64) -> u64,
        timeout_entry: impl Fn(u64) -> u64,
    ) -> Result<(), Error> {
        match &mut self.state {
            MbxWriteReadState::MailboxFull(m) => m.start(
                maindevice,
                retry_count,
                timeout,
                tx_entries,
                sock,
                ring,
                write_mbx,
                read_mbx,
                configured_addr,
                identifier,
                idx,
                write_entry,
                timeout_entry,
            ),
            _ => unreachable!(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn update<'p>(
        &mut self,
        received: ReceivedPdu<'p>,
        header: PduHeader,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        write_mbx: &ethercrab::Mailbox,
        read_mbx: &ethercrab::Mailbox,
        configured_addr: u16,
        identifier: Option<u8>,
        idx: u16,
        write_entry: impl Fn(u64) -> u64,
        timeout_entry: impl Fn(u64) -> u64,
    ) -> Result<Option<(R, ReceivedPdu<'p>)>, Error> {
        match &mut self.state {
            MbxWriteReadState::MailboxFull(m) => {
                if m.update(
                    received,
                    header,
                    maindevice,
                    retry_count,
                    timeout,
                    tx_entries,
                    sock,
                    ring,
                    write_mbx,
                    read_mbx,
                    configured_addr,
                    identifier,
                    idx,
                    &write_entry,
                    &timeout_entry,
                )? {
                    let mut read = CoeRead::new();
                    let mut write = CoeWrite::new();
                    //NOTE: these are the only places that are allowed to have this identifier mask

                    read.start(
                        maindevice,
                        retry_count,
                        timeout,
                        tx_entries,
                        sock,
                        ring,
                        read_mbx,
                        configured_addr,
                        idx,
                        Some(1 | ((!0b11) & identifier.unwrap_or(0))),
                        &write_entry,
                        &timeout_entry,
                    )?;

                    let bytes = self.req.pack();

                    write.start(
                        maindevice,
                        retry_count,
                        timeout,
                        tx_entries,
                        sock,
                        ring,
                        write_mbx,
                        configured_addr,
                        idx,
                        Some(2 | ((!0b11) & identifier.unwrap_or(0))),
                        bytes.as_ref(),
                        &write_entry,
                        &timeout_entry,
                    )?;

                    self.state = MbxWriteReadState::WriteRead { read, write };
                }
            }
            MbxWriteReadState::WriteRead { read, write } => match identifier.map(|id| id & 0b11) {
                Some(2) => {
                    let (_address, _reg) = {
                        let raw = u32::from_le_bytes(header.command_raw);
                        let reg = (raw >> 16) as u16;
                        let addr = raw as u16;
                        (addr, reg)
                    };
                    write.update();
                }

                Some(1) => {
                    let (_address, _reg) = {
                        let raw = u32::from_le_bytes(header.command_raw);
                        let reg = (raw >> 16) as u16;
                        let addr = raw as u16;
                        (addr, reg)
                    };
                    if let Some(bytes) = read.update(
                        received,
                        header,
                        maindevice,
                        retry_count,
                        timeout,
                        tx_entries,
                        sock,
                        ring,
                        read_mbx,
                        configured_addr,
                        identifier,
                        idx,
                        &write_entry,
                        &timeout_entry,
                    )? {
                        let res = ethercrab::SubDevice::parse_coe_service_reponse(bytes, &self.req)
                            .unwrap();

                        return Ok(Some(res));
                    }
                }
                o => unreachable!("{o:?}"),
            },
        }
        Ok(None)
    }
}

#[derive(Debug)]
enum MbxWriteReadState {
    MailboxFull(CoeMailboxState),
    WriteRead { write: CoeWrite, read: CoeRead },
}

#[derive(Debug)]
struct CoeMailboxState {
    rx: ReadMbxState,
    tx: WriteMbxState,
}

impl CoeMailboxState {
    fn new() -> Self {
        Self {
            rx: ReadMbxState::new(),
            tx: WriteMbxState::new(),
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
        ring: &mut IoUring,
        write_mbx: &ethercrab::Mailbox,
        read_mbx: &ethercrab::Mailbox,
        configured_addr: u16,
        identifier: Option<u8>,
        idx: u16,
        write_entry: impl Fn(u64) -> u64,
        timeout_entry: impl Fn(u64) -> u64,
    ) -> Result<(), Error> {
        self.tx.start(
            maindevice,
            retry_count,
            timeout,
            tx_entries,
            sock,
            ring,
            write_mbx,
            configured_addr,
            1 | (identifier.unwrap_or(0) << 2),
            idx,
            &write_entry,
            &timeout_entry,
        )?;
        self.rx.start(
            maindevice,
            retry_count,
            timeout,
            tx_entries,
            sock,
            ring,
            read_mbx,
            configured_addr,
            2 | (identifier.unwrap_or(0) << 2),
            idx,
            write_entry,
            timeout_entry,
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn update(
        &mut self,
        received: ReceivedPdu<'_>,
        header: PduHeader,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        write_mbx: &ethercrab::Mailbox,
        read_mbx: &ethercrab::Mailbox,
        configured_addr: u16,
        identifier: Option<u8>,
        idx: u16,
        write_entry: impl Fn(u64) -> u64,
        timeout_entry: impl Fn(u64) -> u64,
    ) -> Result<bool, Error> {
        // get only the first 2 bits that are used in the read
        match identifier.map(|id| id & 0b11) {
            Some(1) => {
                if self.tx.update(
                    received,
                    header,
                    maindevice,
                    retry_count,
                    timeout,
                    tx_entries,
                    sock,
                    ring,
                    write_mbx,
                    configured_addr,
                    1,
                    idx,
                    write_entry,
                    timeout_entry,
                )? && matches!(self.rx, ReadMbxState::Ready)
                {
                    return Ok(true);
                }
            }
            Some(2) => {
                if self.rx.update(
                    received,
                    header,
                    maindevice,
                    retry_count,
                    timeout,
                    tx_entries,
                    sock,
                    ring,
                    read_mbx,
                    configured_addr,
                    2,
                    idx,
                    write_entry,
                    timeout_entry,
                )? && matches!(self.tx, WriteMbxState::Ready)
                {
                    return Ok(true);
                }
            }
            _ => unreachable!(),
        }
        Ok(false)
    }
}

#[derive(Debug)]
enum CoeWrite {
    Sent,
    Received,
}

impl CoeWrite {
    fn new() -> Self {
        Self::Sent
    }

    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        write_mbx: &ethercrab::Mailbox,
        configured_addr: u16,
        idx: u16,
        identifier: Option<u8>,
        bytes: &[u8],
        write_entry: impl Fn(u64) -> u64,
        timeout_entry: impl Fn(u64) -> u64,
    ) -> Result<(), Error> {
        let (frame, handle) = unsafe {
            maindevice
                .prep_write(configured_addr, write_mbx.address, write_mbx.len, bytes)
                .unwrap()
                .unwrap()
        };

        setup_write(
            frame,
            handle,
            retry_count,
            timeout,
            tx_entries,
            sock,
            ring,
            Some(idx),
            identifier,
            write_entry,
            timeout_entry,
        )
    }

    fn update(&mut self) {
        *self = Self::Received;
    }
}

#[derive(Debug)]
enum CoeRead {
    Empty,
    Ready,
}

impl CoeRead {
    fn new() -> Self {
        Self::Empty
    }

    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        read_mbx: &ethercrab::Mailbox,
        configured_addr: u16,
        idx: u16,
        identifier: Option<u8>,
        write_entry: impl Fn(u64) -> u64,
        timeout_entry: impl Fn(u64) -> u64,
    ) -> Result<(), Error> {
        let (frame, handle) = maindevice
            .prep_mailbox_sync_manager_status(configured_addr, read_mbx.sync_manager)
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
            identifier,
            write_entry,
            timeout_entry,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn update<'p>(
        &mut self,
        received: ReceivedPdu<'p>,
        _header: PduHeader,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        read_mbx: &ethercrab::Mailbox,
        configured_addr: u16,
        identifier: Option<u8>,
        idx: u16,
        write_entry: impl Fn(u64) -> u64,
        timeout_entry: impl Fn(u64) -> u64,
    ) -> Result<Option<ReceivedPdu<'p>>, Error> {
        match self {
            Self::Empty => {
                use ethercrab::EtherCrabWireRead;
                let status =
                    ethercrab::sync_manager_channel::Status::unpack_from_slice(&received).unwrap();

                if !status.mailbox_full {
                    self.start(
                        maindevice,
                        retry_count,
                        timeout,
                        tx_entries,
                        sock,
                        ring,
                        read_mbx,
                        configured_addr,
                        idx,
                        identifier,
                        write_entry,
                        timeout_entry,
                    )?;
                    return Ok(None);
                }

                let (frame, handle) = unsafe {
                    maindevice
                        .prep_read(configured_addr, read_mbx.address, read_mbx.len)
                        .unwrap()
                        .unwrap()
                };
                setup_write(
                    frame,
                    handle,
                    retry_count,
                    timeout,
                    tx_entries,
                    sock,
                    ring,
                    Some(idx),
                    identifier,
                    write_entry,
                    timeout_entry,
                )?;

                *self = Self::Ready;
            }
            Self::Ready => return Ok(Some(received)),
        }
        Ok(None)
    }
}

#[derive(Debug)]
enum WriteMbxState {
    Full,
    Ready,
}

impl WriteMbxState {
    fn new() -> Self {
        Self::Full
    }

    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        write_mbx: &ethercrab::Mailbox,
        configured_addr: u16,
        identifier: u8,
        idx: u16,
        write_entry: impl Fn(u64) -> u64,
        timeout_entry: impl Fn(u64) -> u64,
    ) -> Result<(), Error> {
        let (frame, handle) = maindevice
            .prep_mailbox_sync_manager_status(configured_addr, write_mbx.sync_manager)?
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
            Some(identifier),
            write_entry,
            timeout_entry,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn update(
        &mut self,
        received: ReceivedPdu<'_>,
        _header: PduHeader,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        write_mbx: &ethercrab::Mailbox,
        configured_addr: u16,
        identifier: u8,
        idx: u16,
        write_entry: impl Fn(u64) -> u64,
        timeout_entry: impl Fn(u64) -> u64,
    ) -> Result<bool, Error> {
        match self {
            Self::Full => {
                use ethercrab::EtherCrabWireRead;
                let sm_status =
                    ethercrab::sync_manager_channel::Status::unpack_from_slice(&received).unwrap();

                if sm_status.mailbox_full {
                    self.start(
                        maindevice,
                        retry_count,
                        timeout,
                        tx_entries,
                        sock,
                        ring,
                        write_mbx,
                        configured_addr,
                        identifier,
                        idx,
                        write_entry,
                        timeout_entry,
                    )?;
                    Ok(false)
                } else {
                    *self = Self::Ready;
                    Ok(true)
                }
            }
            _ => unreachable!(),
        }
    }
}

#[derive(Debug)]
enum ReadMbxState {
    Full,
    Flush,
    Ready,
}

impl ReadMbxState {
    fn new() -> Self {
        Self::Full
    }

    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        read_mbx: &ethercrab::Mailbox,
        configured_addr: u16,
        identifier: u8,
        idx: u16,
        write_entry: impl Fn(u64) -> u64,
        timeout_entry: impl Fn(u64) -> u64,
    ) -> Result<(), Error> {
        let (frame, handle) = maindevice
            .prep_mailbox_sync_manager_status(configured_addr, read_mbx.sync_manager)?
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
            Some(identifier),
            write_entry,
            timeout_entry,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn update(
        &mut self,
        received: ReceivedPdu<'_>,
        _header: PduHeader,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        read_mbx: &ethercrab::Mailbox,
        configured_addr: u16,
        identifier: u8,
        idx: u16,
        write_entry: impl Fn(u64) -> u64,
        timeout_entry: impl Fn(u64) -> u64,
    ) -> Result<bool, Error> {
        match self {
            Self::Full => {
                use ethercrab::EtherCrabWireRead;
                let sm_status =
                    ethercrab::sync_manager_channel::Status::unpack_from_slice(&received).unwrap();

                if !sm_status.mailbox_full {
                    *self = Self::Ready;
                    return Ok(true);
                }

                // need to flush whatever is in the rx mailbox of the device
                let (frame, handle) = unsafe {
                    maindevice
                        .prep_read(configured_addr, read_mbx.address, read_mbx.len)
                        .unwrap()
                        .unwrap()
                };

                setup_write(
                    frame,
                    handle,
                    retry_count,
                    timeout,
                    tx_entries,
                    sock,
                    ring,
                    Some(idx),
                    Some(identifier),
                    write_entry,
                    timeout_entry,
                )?;
                *self = Self::Flush;
            }
            Self::Flush => {
                self.start(
                    maindevice,
                    retry_count,
                    timeout,
                    tx_entries,
                    sock,
                    ring,
                    read_mbx,
                    configured_addr,
                    identifier,
                    idx,
                    write_entry,
                    timeout_entry,
                )?;
                *self = Self::Full;
            }
            _ => unreachable!(),
        }
        Ok(false)
    }
}
