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

pub struct PreOp<'a, const N: usize> {
    subdevices: Deque<(SubDevice, PreOpConfigState<'a>), N>,
    configured_count: u16,
}

impl<'a, const N: usize> PreOp<'a, N> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_new<const I: usize, const O: usize, S>(
        subdevs: Deque<(SubDevice, S), N>,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout_duration: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        mut config: impl FnMut(
            ethercrab::SubDeviceRef<'_, &mut ethercrab::SubDevice>,
        ) -> &'a PdoConfig<'a, I, O>,
    ) -> Result<Self, Error> {
        let mut devs = Deque::new();

        for (id, (mut subdev, _)) in subdevs.into_iter().enumerate() {
            let subdev_ref =
                ethercrab::SubDeviceRef::new(maindevice, subdev.configured_address(), &mut subdev);

            let cfg = config(subdev_ref);

            println!("\n\nmoving to pre op\n\n");

            let mut state = PreOpConfigState::new(cfg, &subdev);

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
                id as u16,
            )?;

            let _ = devs.push_back((subdev, state));
        }

        Ok(Self {
            subdevices: devs,
            configured_count: 0,
        })
    }

    #[allow(clippy::too_many_arguments, clippy::type_complexity)]
    pub(crate) fn update<const I: usize, const O: usize>(
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
        mut config: impl FnMut(
            ethercrab::SubDeviceRef<'_, &mut ethercrab::SubDevice>,
        ) -> &'a PdoConfig<'a, I, O>,
        pdi_offset: &mut ethercrab::PdiOffset,
    ) -> Result<Option<(Deque<(SubDevice, PreOpConfigState<'a>), N>, SendRecvIo)>, Error> {
        let idx = idx.unwrap() as usize;
        let (dev, state) = self.subdevices.get_mut(idx).unwrap();

        let subdev = ethercrab::SubDeviceRef::new(maindevice, dev.configured_address(), &mut *dev);

        let cfg = config(subdev);

        if let Some(io) = state.update(
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
        )? {
            self.configured_count += 1;
            if usize::from(self.configured_count) == self.subdevices.len() {
                return Ok(Some((core::mem::take(&mut self.subdevices), io)));
            }
        }
        Ok(None)
    }
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
    ) -> Result<Option<SendRecvIo>, Error> {
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
                    );

                    println!("\n\ngoing to fmmus\n\n");

                    *self = Self::Fmmus(fmmus);
                }
            }
            Self::Fmmus(fmmus) => {
                if let Some((input_len, output_len)) = fmmus.update(
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
                )? {
                    println!("io state: {:?}", subdev.config.io);

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
                )? {
                    return Ok(Some(*io));
                }
            }
        }
        Ok(None)
    }
}
