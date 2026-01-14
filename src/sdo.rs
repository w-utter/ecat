use crate::txbuf::TxBuf;
use ethercrab::{
    EtherCrabWireSized, MainDevice, PduHeader, error::Error, received_frame::ReceivedPdu,
    std::RawSocketDesc,
};
use io_uring::{IoUring, types::Timespec};
use std::collections::BTreeMap;

use crate::mbx::MbxWriteRead;

pub struct SdoRead<T> {
    inner: MbxWriteRead<ethercrab::coe::services::SdoNormal>,
    finished: bool,
    ty: core::marker::PhantomData<T>,
}

impl<T> SdoRead<T> {
    pub fn finished(&self) -> bool {
        self.finished
    }
}

impl<T: ethercrab::EtherCrabWireReadSized> SdoRead<T> {
    pub fn new(mailbox_count: u8, index: u16, subindex: impl Into<ethercrab::SubIndex>) -> Self {
        let req = ethercrab::coe::services::upload(mailbox_count, index, subindex.into());
        let inner = MbxWriteRead::new(req);

        Self {
            inner,
            finished: false,
            ty: core::marker::PhantomData,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn start(
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
        self.inner.start(
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
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update(
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
    ) -> Result<Option<T>, Error> {
        if let Some((header, bytes)) = self.inner.update(
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
            write_entry,
            timeout_entry,
        )? {
            use ethercrab::EtherCrabWireRead;
            let payload = if header.sdo_header.expedited_transfer {
                let len = 4usize.saturating_sub(usize::from(header.sdo_header.size));
                &bytes[..len]
            } else {
                let len = header.header.length.saturating_sub(0x0a);
                let size = u32::unpack_from_slice(&bytes).unwrap();

                let data = bytes.get(u32::PACKED_LEN..).unwrap();
                if size > bytes.len() as u32 {
                    todo!("bad size: {size:?}");
                } else if size < len as u32 {
                    todo!("bad len");
                }

                data.get(..(len as usize)).unwrap()
            };

            let data = T::unpack_from_slice(payload)?;
            self.finished = true;
            return Ok(Some(data));
        }
        Ok(None)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_raw<'p>(
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
    ) -> Result<Option<(ethercrab::coe::services::SdoNormal, ReceivedPdu<'p>)>, Error> {
        self.inner.update(
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
            write_entry,
            timeout_entry,
        )
    }
}

#[derive(Debug)]
pub struct SdoWrite<T> {
    inner: MbxWriteRead<ethercrab::coe::services::SdoExpeditedDownload>,
    ty: core::marker::PhantomData<T>,
}

impl<T: ethercrab::EtherCrabWireWrite + std::fmt::Debug> SdoWrite<T> {
    pub fn new(
        subdev: &ethercrab::SubDevice,
        index: u16,
        subindex: impl Into<ethercrab::SubIndex>,
        data: T,
    ) -> Self {
        if data.packed_len() > 4 {
            todo!("support for larger writes");
        }

        let subindex = subindex.into();

        let mut buf = [0u8; 4];
        data.pack_to_slice(&mut buf).unwrap();

        let req = ethercrab::coe::services::download(
            subdev.mailbox_counter(),
            index,
            subindex,
            buf,
            data.packed_len() as u8,
        );
        let inner = MbxWriteRead::new(req);

        Self {
            inner,
            ty: core::marker::PhantomData,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn start(
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
        self.inner.start(
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
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update<'p>(
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
    ) -> Result<
        Option<(
            ethercrab::coe::services::SdoExpeditedDownload,
            ReceivedPdu<'p>,
        )>,
        Error,
    > {
        if let Some((header, bytes)) = self.inner.update(
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
            write_entry,
            timeout_entry,
        )? {
            return Ok(Some((header, bytes)));
        }
        Ok(None)
    }
}
