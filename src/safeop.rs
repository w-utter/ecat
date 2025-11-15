use crate::txbuf::TxBuf;
use ethercrab::{
    MainDevice, PduHeader, error::Error, received_frame::ReceivedPdu, std::RawSocketDesc,
};
use io_uring::{IoUring, types::Timespec};
use std::collections::BTreeMap;

use crate::state_transition::Transition;

use heapless::Deque;

pub struct SafeOp<const N: usize, U> {
    subdevices: Deque<(U, Transition), N>,
    transition_idx: u16,
}

impl<const N: usize, U: crate::user::UserDevice> SafeOp<N, U> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_new<S1, S2>(
        subdevs: Deque<(U, S1, S2), N>,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout_duration: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        write_entry: impl Fn(u64) -> u64,
    ) -> Result<Self, Error> {
        let mut devs = Deque::new();
        for (subdev, _, _) in subdevs.into_iter() {
            let state = Transition::new(ethercrab::SubDeviceState::Op);
            let _ = devs.push_back((subdev, state));
        }

        let (subdev, state) = devs.front_mut().unwrap();

        state.start(
            maindevice,
            retry_count,
            timeout_duration,
            tx_entries,
            sock,
            ring,
            subdev.subdevice().configured_address(),
            0,
            write_entry,
        )?;

        Ok(Self {
            subdevices: devs,
            transition_idx: 0,
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
        idx: Option<u16>,
        write_entry: impl Fn(u64) -> u64,
    ) -> Result<Option<Deque<(U, Transition), N>>, Error> {
        let idx = idx.unwrap() as usize;
        let (dev, state) = self.subdevices.get_mut(idx).unwrap();
        let configured_addr = dev.subdevice().configured_address();

        if state.update(
            received,
            header,
            maindevice,
            retry_count,
            timeout_duration,
            tx_entries,
            sock,
            ring,
            configured_addr,
            idx as _,
            &write_entry,
        )? {
            self.transition_idx += 1;

            if usize::from(self.transition_idx) != self.subdevices.len() {
                let (subdev, state) = self.subdevices.get_mut(self.transition_idx as _).unwrap();

                state.start(
                    maindevice,
                    retry_count,
                    timeout_duration,
                    tx_entries,
                    sock,
                    ring,
                    subdev.subdevice().configured_address(),
                    self.transition_idx as _,
                    &write_entry,
                )?;

                return Ok(None);
            }
            return Ok(Some(core::mem::take(&mut self.subdevices)));
        }
        Ok(None)
    }
}
