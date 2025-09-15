use crate::txbuf::TxBuf;
use ethercrab::{
    MainDevice, PduHeader, error::Error, received_frame::ReceivedPdu, std::RawSocketDesc,
};
use io_uring::{IoUring, types::Timespec};
use std::collections::BTreeMap;

use crate::pdo::PdoConfig;

pub enum InitState<'a, const MAX_SUBDEVICES: usize, T> {
    Idle,
    Reset(crate::reset::Reset),
    Init(crate::init::Init<MAX_SUBDEVICES>),
    Dc(crate::dc::Dc<MAX_SUBDEVICES>),
    Mbx(crate::mbx_config::MailboxConfig<MAX_SUBDEVICES>),
    PreOp(crate::preop::PreOp<'a, MAX_SUBDEVICES>),
    SafeOp(crate::safeop::SafeOp<MAX_SUBDEVICES>),
    Op(crate::op::Op<MAX_SUBDEVICES, T>),
}

impl<const N: usize, T: Default> Default for InitState<'_, N, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a, const N: usize, T: Default> InitState<'a, N, T> {
    pub fn new() -> Self {
        Self::Idle
    }

    pub fn start(
        &mut self,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
    ) -> Result<(), Error> {
        let mut reset = crate::reset::Reset::new();
        reset.start(maindevice, retry_count, timeout, tx_entries, sock, ring)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update<const I: usize, const O: usize>(
        &mut self,
        received: ReceivedPdu<'_>,
        header: PduHeader,
        maindevice: &mut MainDevice,
        retry_count: usize,
        timeout: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        index: Option<u16>,
        identifier: Option<u8>,
        pdi_offset: &mut ethercrab::PdiOffset,
        config: impl FnMut(
            ethercrab::SubDeviceRef<'_, &mut ethercrab::SubDevice>,
        ) -> &'a PdoConfig<'a, I, O>,
        user_cb: impl FnMut(
            &mut MainDevice,
            &mut ethercrab::SubDevice,
            &mut T,
            Option<(ReceivedPdu<'_>, PduHeader)>,
            &mut BTreeMap<u64, TxBuf>,
            &mut IoUring,
            u16,
            Option<u8>,
        ) -> std::io::Result<()>,
    ) -> Result<(), Error> {
        match self {
            Self::Reset(r) => {
                if let Some(count) = r.update(received, header) {
                    let init = crate::init::Init::start_new(
                        count,
                        maindevice,
                        retry_count,
                        timeout,
                        tx_entries,
                        sock,
                        ring,
                    )?;
                    *self = Self::Init(init);
                }
            }
            Self::Init(i) => {
                if let Some(devs) = i.update(
                    received,
                    header,
                    maindevice,
                    retry_count,
                    timeout,
                    tx_entries,
                    sock,
                    ring,
                    index,
                    identifier,
                )? {
                    let mut dc = crate::dc::Dc::new(devs);

                    dc.start(maindevice, retry_count, timeout, tx_entries, sock, ring)?;
                    *self = Self::Dc(dc);
                }
            }
            Self::Dc(dc) => {
                if let Some(devs) = dc.update(
                    received,
                    header,
                    maindevice,
                    retry_count,
                    timeout,
                    tx_entries,
                    sock,
                    ring,
                    index,
                )? {
                    let mbx_config = crate::mbx_config::MailboxConfig::start_new(
                        devs,
                        maindevice,
                        retry_count,
                        timeout,
                        tx_entries,
                        sock,
                        ring,
                    )?;
                    *self = Self::Mbx(mbx_config);
                }
            }
            Self::Mbx(m) => {
                if let Some(devs) = m.update(
                    received,
                    header,
                    maindevice,
                    retry_count,
                    timeout,
                    tx_entries,
                    sock,
                    ring,
                    identifier,
                    index,
                )? {
                    let preop = crate::preop::PreOp::start_new(
                        devs,
                        maindevice,
                        retry_count,
                        timeout,
                        tx_entries,
                        sock,
                        ring,
                        config,
                    )?;

                    *self = Self::PreOp(preop);
                }
            }
            Self::PreOp(p) => {
                if let Some(devs) = p.update(
                    received,
                    header,
                    maindevice,
                    retry_count,
                    timeout,
                    tx_entries,
                    sock,
                    ring,
                    identifier,
                    index,
                    config,
                    pdi_offset,
                )? {
                    let safeop = crate::safeop::SafeOp::start_new(
                        devs,
                        maindevice,
                        retry_count,
                        timeout,
                        tx_entries,
                        sock,
                        ring,
                    )?;

                    *self = Self::SafeOp(safeop);
                }
            }
            Self::SafeOp(o) => {
                if let Some(devs) = o.update(
                    received,
                    header,
                    maindevice,
                    retry_count,
                    timeout,
                    tx_entries,
                    sock,
                    ring,
                    index,
                )? {
                    let op = crate::op::Op::start_new(devs, maindevice, tx_entries, ring, user_cb)?;

                    *self = Self::Op(op);
                }
            }
            Self::Op(o) => {
                o.update(
                    received, header, maindevice, tx_entries, ring, identifier, index, user_cb,
                )?;
            }
            _ => todo!(),
        }
        Ok(())
    }
}
