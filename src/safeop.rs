use crate::txbuf::TxBuf;
use ethercrab::{
    MainDevice, PduHeader, SubDevice, error::Error, received_frame::ReceivedPdu, std::RawSocketDesc,
};
use io_uring::{IoUring, types::Timespec};
use std::collections::BTreeMap;

use crate::state_transition::Transition;

use heapless::Deque;

pub struct SafeOp<const N: usize> {
    subdevices: Deque<(SubDevice, Transition), N>,
    transition_count: u16,
}

impl<const N: usize> SafeOp<N> {
    pub(crate) fn start_new<S>(
        subdevs: Deque<(SubDevice, S), N>,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout_duration: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
    ) -> Result<Self, Error> {
        println!("\n\nmoving to op\n\n");

        let mut devs = Deque::new();
        for (id, (subdev, _)) in subdevs.into_iter().enumerate() {
            let mut state = Transition::new(ethercrab::SubDeviceState::Op);

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
        idx: Option<u16>,
    ) -> Result<Option<Deque<(SubDevice, Transition), N>>, Error> {
        let idx = idx.unwrap() as usize;
        let (dev, state) = self.subdevices.get_mut(idx).unwrap();
        let configured_addr = dev.configured_address();

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
        )? {
            self.transition_count += 1;

            if usize::from(self.transition_count) == self.subdevices.len() {
                return Ok(Some(core::mem::take(&mut self.subdevices)));
            }
        }
        Ok(None)
    }
}
