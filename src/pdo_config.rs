use ethercrab::{
    Mailbox, MainDevice, PduHeader, SubDevice, error::Error, received_frame::ReceivedPdu,
    std::RawSocketDesc,
};

use crate::pdo::{PdoConfig, PdoObject};
use crate::txbuf::TxBuf;
use io_uring::{IoUring, types::Timespec};
use std::collections::BTreeMap;

use crate::sdo::SdoWrite;

pub(crate) struct PdoMappingConfig<'a> {
    state: PdoConfigState<'a>,
}

impl<'a> PdoMappingConfig<'a> {
    pub(crate) fn new<const I: usize, const O: usize>(
        config: &'a PdoConfig<'a, I, O>,
        subdev: &SubDevice,
    ) -> Self {
        let input = config
            .inputs
            .first()
            .map(|input| (input.start_map(subdev), 0));

        Self {
            state: PdoConfigState::Sdo {
                input,
                output: None,
            },
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
        write_mbx: &Mailbox,
        read_mbx: &Mailbox,
        configured_addr: u16,
        identifier: Option<u8>,
        idx: u16,
    ) -> Result<(), Error> {
        match &mut self.state {
            PdoConfigState::Sdo { input, output } => {
                if let Some((input, _)) = input.as_mut() {
                    input.start(
                        maindevice,
                        retry_count,
                        timeout,
                        tx_entries,
                        sock,
                        ring,
                        write_mbx,
                        read_mbx,
                        configured_addr,
                        Some(1 | (identifier.unwrap_or(0) << 2)),
                        idx,
                    )?;
                }

                if let Some((output, _)) = output.as_mut() {
                    output.start(
                        maindevice,
                        retry_count,
                        timeout,
                        tx_entries,
                        sock,
                        ring,
                        write_mbx,
                        read_mbx,
                        configured_addr,
                        Some(2 | (identifier.unwrap_or(0) << 2)),
                        idx,
                    )?;
                }
            }
            _ => unreachable!(),
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn update<const I: usize, const O: usize>(
        &mut self,
        received: ReceivedPdu<'_>,
        header: PduHeader,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        write_mbx: &Mailbox,
        read_mbx: &Mailbox,
        configured_addr: u16,
        identifier: Option<u8>,
        idx: u16,
        subdev: &SubDevice,
        config: &'a PdoConfig<'a, I, O>,
    ) -> Result<bool, Error> {
        // this is a mess.
        match &mut self.state {
            PdoConfigState::Sdo { input, output } => {
                macro_rules! to_sync_managers {
                    () => {
                        let input = if !config.inputs.is_empty() {
                            let mut write = SdoWrite::new(subdev, 0x1c10 + 2, 0, 0);
                            write.start(
                                maindevice,
                                retry_count,
                                timeout,
                                tx_entries,
                                sock,
                                ring,
                                write_mbx,
                                read_mbx,
                                configured_addr,
                                Some(1),
                                idx,
                            )?;
                            Some((PdoMapState::Clear(write), 0))
                        } else {
                            None
                        };

                        self.state = PdoConfigState::SyncManagers {
                            input,
                            output: None,
                        };
                    };
                }

                match identifier.map(|id| (id >> 2) & 0b11) {
                    Some(1) => {
                        let (uinput, input_idx) = input.as_mut().unwrap();
                        if uinput.update(
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
                            subdev,
                        )? {
                            *input_idx += 1;

                            if let Some(cfg) = config.inputs.get(*input_idx as usize) {
                                let mut map = cfg.start_map(subdev);

                                map.start(
                                    maindevice,
                                    retry_count,
                                    timeout,
                                    tx_entries,
                                    sock,
                                    ring,
                                    write_mbx,
                                    read_mbx,
                                    configured_addr,
                                    Some(1),
                                    idx,
                                )?;

                                *uinput = map;
                            } else {
                                *input = None;

                                *output = config
                                    .outputs
                                    .first()
                                    .map(|output| (output.start_map(subdev), 0));
                                // NOTE: could not get it working in parallel, so this does it
                                // sequentially
                                match output {
                                    Some((o, _)) => {
                                        o.start(
                                            maindevice,
                                            retry_count,
                                            timeout,
                                            tx_entries,
                                            sock,
                                            ring,
                                            write_mbx,
                                            read_mbx,
                                            configured_addr,
                                            Some(2),
                                            idx,
                                        )?;
                                    }
                                    None => {
                                        to_sync_managers!();
                                    }
                                }
                            }
                        }
                    }
                    Some(2) => {
                        let (uoutput, output_idx) = output.as_mut().unwrap();
                        if uoutput.update(
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
                            subdev,
                        )? {
                            *output_idx += 1;

                            if let Some(cfg) = config.inputs.get(*output_idx as usize) {
                                let mut map = cfg.start_map(subdev);

                                map.start(
                                    maindevice,
                                    retry_count,
                                    timeout,
                                    tx_entries,
                                    sock,
                                    ring,
                                    write_mbx,
                                    read_mbx,
                                    configured_addr,
                                    Some(2),
                                    idx,
                                )?;

                                *uoutput = map;
                            } else {
                                *output = None;

                                if input.is_none() {
                                    to_sync_managers!();
                                }
                            }
                        }
                    }
                    _ => unreachable!(),
                }
            }
            PdoConfigState::SyncManagers { input, output } => {
                match identifier.map(|id| (id >> 2) & 0b11) {
                    Some(1) => {
                        let (uinput, input_idx) = input.as_mut().unwrap();

                        match uinput {
                            PdoMapState::Clear(c) => {
                                if c.update(
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
                                )?
                                .is_some()
                                {
                                    let input = config.inputs.first().unwrap();
                                    let mut s = SdoWrite::new(subdev, 0x1c10 + 2, 1, input.index);
                                    s.start(
                                        maindevice,
                                        retry_count,
                                        timeout,
                                        tx_entries,
                                        sock,
                                        ring,
                                        write_mbx,
                                        read_mbx,
                                        configured_addr,
                                        Some(1),
                                        idx,
                                    )?;

                                    *uinput = PdoMapState::Map(s, 0);
                                }
                            }
                            PdoMapState::Map(w, count) => {
                                if w.update(
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
                                )?
                                .is_some()
                                {
                                    *count += 1;
                                    if let Some(input) = config.inputs.get(*count as usize) {
                                        let mut s = SdoWrite::new(
                                            subdev,
                                            0x1c10 + 2 + (*count as u16),
                                            *count + 1,
                                            input.index,
                                        );
                                        s.start(
                                            maindevice,
                                            retry_count,
                                            timeout,
                                            tx_entries,
                                            sock,
                                            ring,
                                            write_mbx,
                                            read_mbx,
                                            configured_addr,
                                            Some(1),
                                            idx,
                                        )?;
                                        *w = s;
                                    } else {
                                        let mut s = SdoWrite::new(
                                            subdev,
                                            0x1c10 + 2,
                                            0,
                                            config.inputs.len() as u8,
                                        );

                                        s.start(
                                            maindevice,
                                            retry_count,
                                            timeout,
                                            tx_entries,
                                            sock,
                                            ring,
                                            write_mbx,
                                            read_mbx,
                                            configured_addr,
                                            Some(1),
                                            idx,
                                        )?;

                                        *uinput = PdoMapState::SetCount(s);
                                    }
                                }
                            }
                            PdoMapState::SetCount(c) => {
                                if c.update(
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
                                )?
                                .is_some()
                                {
                                    *input_idx += 1;
                                    if config.inputs.get(*input_idx as usize).is_some() {
                                        let mut write =
                                            SdoWrite::new(subdev, 0x1c10 + 2 + *input_idx, 0, 0);
                                        write.start(
                                            maindevice,
                                            retry_count,
                                            timeout,
                                            tx_entries,
                                            sock,
                                            ring,
                                            write_mbx,
                                            read_mbx,
                                            configured_addr,
                                            Some(1),
                                            idx,
                                        )?;

                                        *uinput = PdoMapState::Clear(write);
                                    } else {
                                        *input = None;

                                        *output = if !config.outputs.is_empty() {
                                            let mut write = SdoWrite::new(
                                                subdev,
                                                0x1c10 + 2 + (config.inputs.len() as u16),
                                                0,
                                                0,
                                            );
                                            // NOTE: same as before, to do sequentially as parallel
                                            // didnt work
                                            write.start(
                                                maindevice,
                                                retry_count,
                                                timeout,
                                                tx_entries,
                                                sock,
                                                ring,
                                                write_mbx,
                                                read_mbx,
                                                configured_addr,
                                                Some(2),
                                                idx,
                                            )?;
                                            Some((PdoMapState::Clear(write), 0))
                                        } else {
                                            None
                                        };

                                        if output.is_none() {
                                            return Ok(true);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Some(2) => {
                        let (uoutput, output_idx) = output.as_mut().unwrap();

                        match uoutput {
                            PdoMapState::Clear(c) => {
                                if c.update(
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
                                )?
                                .is_some()
                                {
                                    let output = config.outputs.first().unwrap();
                                    let mut s = SdoWrite::new(
                                        subdev,
                                        0x1c10 + 2 + (config.inputs.len() as u16),
                                        1,
                                        output.index,
                                    );
                                    s.start(
                                        maindevice,
                                        retry_count,
                                        timeout,
                                        tx_entries,
                                        sock,
                                        ring,
                                        write_mbx,
                                        read_mbx,
                                        configured_addr,
                                        Some(2),
                                        idx,
                                    )?;

                                    *uoutput = PdoMapState::Map(s, 0);
                                }
                            }
                            PdoMapState::Map(w, count) => {
                                if w.update(
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
                                )?
                                .is_some()
                                {
                                    *count += 1;
                                    if let Some(output) = config.outputs.get(*count as usize) {
                                        let mut s = SdoWrite::new(
                                            subdev,
                                            0x1c10
                                                + 2
                                                + (*count as u16)
                                                + (config.inputs.len() as u16),
                                            *count + 1,
                                            output.index,
                                        );
                                        s.start(
                                            maindevice,
                                            retry_count,
                                            timeout,
                                            tx_entries,
                                            sock,
                                            ring,
                                            write_mbx,
                                            read_mbx,
                                            configured_addr,
                                            Some(2),
                                            idx,
                                        )?;
                                        *w = s;
                                    } else {
                                        let mut s = SdoWrite::new(
                                            subdev,
                                            0x1c10 + 2 + (config.inputs.len() as u16),
                                            0,
                                            config.outputs.len() as u8,
                                        );

                                        s.start(
                                            maindevice,
                                            retry_count,
                                            timeout,
                                            tx_entries,
                                            sock,
                                            ring,
                                            write_mbx,
                                            read_mbx,
                                            configured_addr,
                                            Some(2),
                                            idx,
                                        )?;

                                        *uoutput = PdoMapState::SetCount(s);
                                    }
                                }
                            }
                            PdoMapState::SetCount(c) => {
                                if c.update(
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
                                )?
                                .is_some()
                                {
                                    *output_idx += 1;
                                    if config.outputs.get(*output_idx as usize).is_some() {
                                        let mut write = SdoWrite::new(
                                            subdev,
                                            0x1c10 + 2 + *output_idx + (config.inputs.len() as u16),
                                            0,
                                            0,
                                        );
                                        write.start(
                                            maindevice,
                                            retry_count,
                                            timeout,
                                            tx_entries,
                                            sock,
                                            ring,
                                            write_mbx,
                                            read_mbx,
                                            configured_addr,
                                            Some(2),
                                            idx,
                                        )?;

                                        *uoutput = PdoMapState::Clear(write);
                                    } else {
                                        *output = None;

                                        if input.is_none() {
                                            return Ok(true);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => unreachable!(),
                }
            }
        }
        Ok(false)
    }
}

enum PdoConfigState<'a> {
    Sdo {
        input: Option<(PdoMap<'a>, u16)>,
        output: Option<(PdoMap<'a>, u16)>,
    },
    SyncManagers {
        input: Option<(PdoMapState<u16>, u16)>,
        output: Option<(PdoMapState<u16>, u16)>,
    },
}

#[derive(Debug)]
pub(crate) enum PdoMapState<T = u32> {
    Clear(SdoWrite<u8>),
    Map(SdoWrite<T>, u8),
    SetCount(SdoWrite<u8>),
}

#[derive(Debug)]
pub(crate) struct PdoMap<'a> {
    pub(crate) index: u16,
    pub(crate) objects: &'a [PdoObject],
    pub(crate) state: PdoMapState,
}

impl<'a> PdoMap<'a> {
    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        write_mbx: &Mailbox,
        read_mbx: &Mailbox,
        configured_addr: u16,
        identifier: Option<u8>,
        idx: u16,
    ) -> Result<(), Error> {
        match &mut self.state {
            PdoMapState::Clear(c) => c.start(
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
            ),
            _ => unreachable!(),
        }
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
        write_mbx: &Mailbox,
        read_mbx: &Mailbox,
        configured_addr: u16,
        identifier: Option<u8>,
        idx: u16,
        subdev: &SubDevice,
    ) -> Result<bool, Error> {
        match &mut self.state {
            PdoMapState::Clear(c) => {
                if c.update(
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
                )?
                .is_some()
                {
                    if let Some(obj) = self.objects.first() {
                        let mut s = SdoWrite::new(subdev, self.index, 1, obj.0);
                        s.start(
                            maindevice,
                            retry_count,
                            timeout,
                            tx_entries,
                            sock,
                            ring,
                            write_mbx,
                            read_mbx,
                            configured_addr,
                            identifier.map(|id| id >> 2),
                            idx,
                        )?;
                        self.state = PdoMapState::Map(s, 0);
                    } else {
                        return Ok(true);
                    }
                }
            }
            PdoMapState::Map(s, subidx) => {
                if s.update(
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
                )?
                .is_some()
                {
                    *subidx += 1;
                    if let Some(obj) = self.objects.get(*subidx as usize) {
                        let mut new_s = SdoWrite::new(subdev, self.index, *subidx + 1, obj.0);
                        new_s.start(
                            maindevice,
                            retry_count,
                            timeout,
                            tx_entries,
                            sock,
                            ring,
                            write_mbx,
                            read_mbx,
                            configured_addr,
                            identifier.map(|id| id >> 2),
                            idx,
                        )?;
                        *s = new_s;
                    } else {
                        let mut s = SdoWrite::new(subdev, self.index, 0, self.objects.len() as u8);

                        s.start(
                            maindevice,
                            retry_count,
                            timeout,
                            tx_entries,
                            sock,
                            ring,
                            write_mbx,
                            read_mbx,
                            configured_addr,
                            identifier.map(|id| id >> 2),
                            idx,
                        )?;

                        self.state = PdoMapState::SetCount(s);
                    }
                }
            }
            PdoMapState::SetCount(s) => {
                if s.update(
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
                )?
                .is_some()
                {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}
