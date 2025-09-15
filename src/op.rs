use crate::txbuf::TxBuf;
use ethercrab::{MainDevice, PduHeader, SubDevice, error::Error, received_frame::ReceivedPdu};
use io_uring::IoUring;
use std::collections::BTreeMap;

use heapless::Deque;

pub struct Op<const N: usize, T> {
    subdevices: Deque<(SubDevice, T), N>,
}

impl<const N: usize, T: Default> Op<N, T> {
    pub(crate) fn start_new<S>(
        subdevs: Deque<(SubDevice, S), N>,
        maindevice: &mut MainDevice,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        ring: &mut IoUring,
        mut user_cb: impl FnMut(
            &mut MainDevice,
            &mut ethercrab::SubDevice,
            &mut T,
            Option<(ReceivedPdu<'_>, PduHeader)>,
            &mut BTreeMap<u64, TxBuf>,
            &mut IoUring,
            u16,
            Option<u8>,
        ) -> std::io::Result<()>,
    ) -> Result<Self, Error> {
        let mut subdevices = Deque::new();
        for (id, (mut subdev, _)) in subdevs.into_iter().enumerate() {
            let mut state = T::default();
            user_cb(
                maindevice,
                &mut subdev,
                &mut state,
                None,
                tx_entries,
                ring,
                id as _,
                None,
            )
            .map_err(|_| Error::Internal)?;
            let _ = subdevices.push_back((subdev, state));
        }

        Ok(Self { subdevices })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn update(
        &mut self,
        received: ReceivedPdu<'_>,
        header: PduHeader,
        maindevice: &mut MainDevice,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        ring: &mut IoUring,
        identifier: Option<u8>,
        idx: Option<u16>,
        mut user_cb: impl FnMut(
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
        let idx = idx.unwrap() as usize;
        let (dev, state) = self.subdevices.get_mut(idx).unwrap();

        user_cb(
            maindevice,
            dev,
            state,
            Some((received, header)),
            tx_entries,
            ring,
            idx as _,
            identifier,
        )
        .map_err(|_| Error::Internal)?;
        Ok(())
    }
}
