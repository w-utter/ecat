use crate::txbuf::TxBuf;
use ethercrab::{
    MainDevice, PduHeader, SubDevice, error::Error, received_frame::ReceivedPdu, std::RawSocketDesc,
};
use io_uring::{IoUring, types::Timespec};
use std::collections::BTreeMap;

use crate::pdo::PdoConfig;
use crate::state_transition::Transition;

use crate::fmmu::ConfigureFmmus;
use crate::pdo_config::PdoMappingConfig;

use heapless::Deque;

pub struct PreOp<'a, const N: usize, const I: usize, const O: usize, U> {
    subdevices: Deque<(U, &'a PdoConfig<'a, I, O>, PreOpConfigState<'a>), N>,
    configured_input_idx: u16,
    configured_output_idx: u16,
}

impl<'a, const N: usize, const I: usize, const O: usize, U: crate::user::UserDevice>
    PreOp<'a, N, I, O, U>
{
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_new<S>(
        subdevs: Deque<(SubDevice, S), N>,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout_duration: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        mut config: impl FnMut(&MainDevice, ethercrab::SubDevice) -> (U, &'a PdoConfig<'a, I, O>),
        write_entry: impl Fn(u64) -> u64,
    ) -> Result<Self, Error> {
        let mut devs = Deque::new();

        for (subdev, _) in subdevs.into_iter() {
            let (dev, cfg) = config(maindevice, subdev);
            let state = PreOpConfigState::new(cfg, dev.subdevice());

            let _ = devs.push_back((dev, cfg, state));
        }

        let (dev, _, state) = devs.front_mut().unwrap();
        let subdev = dev.subdevice_mut();

        state.start(
            maindevice,
            retry_count,
            timeout_duration,
            tx_entries,
            sock,
            ring,
            &subdev.config.mailbox.write.unwrap(),
            &subdev.config.mailbox.read.unwrap(),
            subdev.configured_address(),
            0,
            write_entry,
        )?;

        Ok(Self {
            subdevices: devs,
            configured_input_idx: 0,
            configured_output_idx: 0,
        })
    }

    #[allow(clippy::too_many_arguments, clippy::type_complexity)]
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
        pdi_offset: &mut ethercrab::PdiOffset,
        write_entry: impl Fn(u64) -> u64,
    ) -> Result<
        Option<(
            Deque<(U, &'a PdoConfig<'a, I, O>, PreOpConfigState<'a>), N>,
            SendRecvIo,
        )>,
        Error,
    > {
        let idx = idx.unwrap() as usize;
        let (dev, cfg, state) = self.subdevices.get_mut(idx).unwrap();
        let dev = dev.subdevice_mut();

        if let Some(mapping) = state.update(
            received,
            header,
            maindevice,
            retry_count,
            timeout_duration,
            tx_entries,
            sock,
            ring,
            &dev.config.mailbox.write.unwrap(),
            &dev.config.mailbox.read.unwrap(),
            dev.configured_address(),
            idx as u16,
            dev,
            identifier,
            cfg,
            pdi_offset,
            &write_entry,
        )? {
            let (configured_idx, start) =
                if usize::from(self.configured_input_idx) == self.subdevices.len() {
                    (&mut self.configured_output_idx, false)
                } else {
                    (&mut self.configured_input_idx, true)
                };

            *configured_idx += 1;

            if usize::from(*configured_idx) != self.subdevices.len() {
                let (dev, _, state) = self.subdevices.get_mut(*configured_idx as _).unwrap();
                let subdev = dev.subdevice_mut();

                if start {
                    state.start(
                        maindevice,
                        retry_count,
                        timeout_duration,
                        tx_entries,
                        sock,
                        ring,
                        &subdev.config.mailbox.write.unwrap(),
                        &subdev.config.mailbox.read.unwrap(),
                        subdev.configured_address(),
                        *configured_idx as _,
                        &write_entry,
                    )?;
                } else {
                    match state {
                        PreOpConfigState::Fmmus(f) => f.start_output(
                            maindevice,
                            retry_count,
                            timeout_duration,
                            tx_entries,
                            sock,
                            ring,
                            subdev.configured_address(),
                            *configured_idx as _,
                            &write_entry,
                        )?,
                        _ => unreachable!(),
                    }
                }
                return Ok(None);
            } else if start {
                let (dev, _, state) = self.subdevices.front_mut().unwrap();
                let subdev = dev.subdevice_mut();
                match state {
                    PreOpConfigState::Fmmus(f) => f.start_output(
                        maindevice,
                        retry_count,
                        timeout_duration,
                        tx_entries,
                        sock,
                        ring,
                        subdev.configured_address(),
                        0,
                        &write_entry,
                    )?,
                    _ => unreachable!(),
                }
            } else if let FmmuMapping::Output(io) = mapping {
                return Ok(Some((core::mem::take(&mut self.subdevices), io)));
            }
        }
        Ok(None)
    }
}

#[derive(Debug)]
pub(crate) enum FmmuMapping<T> {
    Input,
    Output(T),
}

#[derive(Clone, Copy, Debug)]
pub struct SendRecvIo {
    input_end: usize,
    output_end: usize,
}

impl SendRecvIo {
    pub fn output_len(&self) -> usize {
        self.output_end - self.input_end
    }

    pub fn input_len(&self) -> usize {
        self.input_end
    }
}

#[allow(clippy::large_enum_variant)]
pub(crate) enum PreOpConfigState<'a> {
    Pdos(PdoMappingConfig<'a>),
    Fmmus(ConfigureFmmus),
    SafeOpTransition(Transition, SendRecvIo),
}

impl<'a> PreOpConfigState<'a> {
    fn new<const I: usize, const O: usize>(
        config: &'a PdoConfig<'a, I, O>,
        subdev: &ethercrab::SubDevice,
    ) -> Self {
        Self::Pdos(PdoMappingConfig::new(config, subdev))
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
        write_mbx: &ethercrab::Mailbox,
        read_mbx: &ethercrab::Mailbox,
        configured_addr: u16,
        idx: u16,
        write_entry: impl Fn(u64) -> u64,
    ) -> Result<(), Error> {
        match self {
            Self::Pdos(pdos) => pdos.start(
                maindevice,
                retry_count,
                timeout_duration,
                tx_entries,
                sock,
                ring,
                write_mbx,
                read_mbx,
                configured_addr,
                None,
                idx,
                write_entry,
            ),
            _ => unreachable!(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn update<const I: usize, const O: usize>(
        &mut self,
        received: ReceivedPdu<'_>,
        header: PduHeader,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout_duration: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        write_mbx: &ethercrab::Mailbox,
        read_mbx: &ethercrab::Mailbox,
        configured_addr: u16,
        idx: u16,
        subdev: &mut ethercrab::SubDevice,
        identifier: Option<u8>,
        config: &'a PdoConfig<'a, I, O>,
        pdi_offset: &mut ethercrab::PdiOffset,
        write_entry: impl Fn(u64) -> u64,
    ) -> Result<Option<FmmuMapping<SendRecvIo>>, Error> {
        match self {
            Self::Pdos(pdos) => {
                if pdos.update(
                    received,
                    header,
                    maindevice,
                    retry_count,
                    timeout_duration,
                    tx_entries,
                    sock,
                    ring,
                    write_mbx,
                    read_mbx,
                    configured_addr,
                    identifier,
                    idx,
                    subdev,
                    config,
                    &write_entry,
                )? {
                    let mut fmmus = ConfigureFmmus::new();
                    fmmus.start(
                        maindevice,
                        retry_count,
                        timeout_duration,
                        tx_entries,
                        sock,
                        ring,
                        configured_addr,
                        idx,
                        &write_entry,
                    );

                    *self = Self::Fmmus(fmmus);
                }
            }
            Self::Fmmus(fmmus) => {
                if let Some(res) = fmmus.update(
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
                    subdev,
                    identifier,
                    config,
                    pdi_offset,
                    &write_entry,
                )? {
                    let (input_len, output_len) = match res {
                        FmmuMapping::Input => return Ok(Some(FmmuMapping::Input)),
                        FmmuMapping::Output(len) => len,
                    };

                    let mut state = Transition::new(ethercrab::SubDeviceState::SafeOp);
                    state.start(
                        maindevice,
                        retry_count,
                        timeout_duration,
                        tx_entries,
                        sock,
                        ring,
                        configured_addr,
                        idx,
                        &write_entry,
                    )?;
                    *self = Self::SafeOpTransition(
                        state,
                        SendRecvIo {
                            input_end: input_len,
                            output_end: output_len,
                        },
                    );
                }
            }
            Self::SafeOpTransition(transition, io) => {
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
                    &write_entry,
                )? {
                    return Ok(Some(FmmuMapping::Output(*io)));
                }
            }
        }
        Ok(None)
    }
}
