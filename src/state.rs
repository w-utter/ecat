use crate::txbuf::TxBuf;
use ethercrab::{
    MainDevice, PduHeader, error::Error, received_frame::ReceivedPdu, std::RawSocketDesc,
};
use io_uring::{IoUring, types::Timespec};
use std::collections::BTreeMap;

use crate::pdo::PdoConfig;

pub enum InitState<'a, const MAX_SUBDEVICES: usize, const I: usize, const O: usize, U> {
    Idle,

    //TODO: all of the earlier stuff (resetting, configuration, etc) needs to be done in
    //series.
    Reset(crate::reset::Reset),
    Init(crate::init::Init<MAX_SUBDEVICES>),
    Dc(crate::dc::Dc<MAX_SUBDEVICES>),
    Mbx(crate::mbx_config::MailboxConfig<MAX_SUBDEVICES>),
    PreOp(crate::preop::PreOp<'a, MAX_SUBDEVICES, I, O, U>),
    SafeOp(
        crate::safeop::SafeOp<MAX_SUBDEVICES, U>,
        crate::preop::SendRecvIo,
    ),
    Op(crate::op::Op<MAX_SUBDEVICES, U>, SendCtx),
}

pub struct SendCtx {
    send_bytes: Vec<u8>,
    input_offset: usize,
}

impl From<crate::preop::SendRecvIo> for SendCtx {
    fn from(f: crate::preop::SendRecvIo) -> SendCtx {
        Self {
            send_bytes: vec![0; f.output_len() + f.input_len()],
            input_offset: f.input_len(),
        }
    }
}

impl<const N: usize, const I: usize, const O: usize, U> Default for InitState<'_, N, I, O, U> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize, const I: usize, const O: usize, U> InitState<'_, N, I, O, U> {
    pub fn new() -> Self {
        Self::Idle
    }
}

impl<'a, const N: usize, const I: usize, const O: usize, U: crate::user::UserDevice>
    InitState<'a, N, I, O, U>
{
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        &mut self,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        write_entry: impl Fn(u64) -> u64,
    ) -> Result<(), Error> {
        let mut reset = crate::reset::Reset::new();
        reset.start(
            maindevice,
            retry_count,
            timeout,
            tx_entries,
            sock,
            ring,
            write_entry,
        )?;
        *self = Self::Reset(reset);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update(
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
        config: impl FnMut(&MainDevice, ethercrab::SubDevice) -> (U, &'a PdoConfig<'a, I, O>),
        user_cb: impl FnMut(
            &mut MainDevice,
            &mut U,
            Option<crate::op::DeviceResponse<'_, '_>>,
            &mut BTreeMap<u64, TxBuf>,
            &mut IoUring,
            u16,
            Option<u8>,
            &mut [u8],
        ) -> std::io::Result<Option<crate::user::ControlFlow>>,
        write_entry: impl Fn(u64) -> u64,
    ) -> Result<(), Error> {
        match self {
            Self::Reset(r) => {
                if let Some(count) = r.update(
                    received,
                    header,
                    maindevice,
                    retry_count,
                    timeout,
                    tx_entries,
                    sock,
                    ring,
                    &write_entry,
                )? {
                    let init = crate::init::Init::start_new(
                        count,
                        maindevice,
                        retry_count,
                        timeout,
                        tx_entries,
                        sock,
                        ring,
                        &write_entry,
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
                    &write_entry,
                )? {
                    let mut dc = crate::dc::Dc::new(devs);

                    dc.start(
                        maindevice,
                        retry_count,
                        timeout,
                        tx_entries,
                        sock,
                        ring,
                        &write_entry,
                    )?;
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
                    &write_entry,
                )? {
                    let mbx_config = crate::mbx_config::MailboxConfig::start_new(
                        devs,
                        maindevice,
                        retry_count,
                        timeout,
                        tx_entries,
                        sock,
                        ring,
                        &write_entry,
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
                    &write_entry,
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
                        &write_entry,
                    )?;

                    *self = Self::PreOp(preop);
                }
            }
            Self::PreOp(p) => {
                if let Some((devs, io)) = p.update(
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
                    pdi_offset,
                    &write_entry,
                )? {
                    let safeop = crate::safeop::SafeOp::start_new(
                        devs,
                        maindevice,
                        retry_count,
                        timeout,
                        tx_entries,
                        sock,
                        ring,
                        &write_entry,
                    )?;

                    *self = Self::SafeOp(safeop, io);
                }
            }
            Self::SafeOp(o, io) => {
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
                    &write_entry,
                )? {
                    let mut io: SendCtx = (*io).into();

                    let op = crate::op::Op::start_new(
                        devs,
                        maindevice,
                        tx_entries,
                        ring,
                        user_cb,
                        &mut io.send_bytes,
                        retry_count,
                        timeout,
                        sock,
                        &write_entry,
                    )?;

                    *self = Self::Op(op, io);
                }
            }
            Self::Op(o, io) => {
                if let Some(flow) = o.update(
                    received,
                    header,
                    maindevice,
                    tx_entries,
                    ring,
                    identifier,
                    index,
                    user_cb,
                    io.input_offset,
                    &mut io.send_bytes,
                    retry_count,
                    timeout,
                    sock,
                    &write_entry,
                )? {
                    use crate::user::ControlFlow;
                    match flow {
                        ControlFlow::Restart => {
                            *self = Self::Idle;
                            self.start(
                                maindevice,
                                retry_count,
                                timeout,
                                tx_entries,
                                sock,
                                ring,
                                &write_entry,
                            )?;
                        }
                    }
                }
            }
            _ => todo!(),
        }
        Ok(())
    }
}
