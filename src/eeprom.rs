use crate::setup::setup_write;
use crate::txbuf::TxBuf;
use ethercrab::{
    MainDevice, PduHeader, error::Error, received_frame::ReceivedPdu, std::RawSocketDesc,
};
use io_uring::{IoUring, types::Timespec};
use std::collections::BTreeMap;

use range::RangeReader;
use read_state::RegisterReadState;

#[derive(Debug)]
pub(crate) struct Found {
    pub(crate) start: u16,
    pub(crate) len: u16,
}

pub(crate) mod read_state {
    pub use super::*;
    pub(crate) enum RegisterReadState {
        RequestRead,
        WaitForDevice,
        ReadData,
    }

    impl RegisterReadState {
        #[allow(clippy::too_many_arguments)]
        pub(crate) fn start(
            &mut self,
            maindevice: &MainDevice,
            retry_count: usize,
            timeout_duration: &Timespec,
            tx_entries: &mut BTreeMap<u64, TxBuf>,
            sock: &RawSocketDesc,
            ring: &mut IoUring,
            configured_addr: u16,
            start_addr: u16,
            idx: u16,
            identifier: u8,
            write_entry: impl Fn(u64) -> u64,
            timeout_entry: impl Fn(u64) -> u64,
        ) -> Result<(), Error> {
            let (frame, handle) = maindevice
                .prep_read_eeprom_chunk(configured_addr, start_addr)?
                .unwrap();
            setup_write(
                frame,
                handle,
                retry_count,
                timeout_duration,
                tx_entries,
                sock,
                ring,
                Some(idx),
                Some(identifier),
                &write_entry,
                &timeout_entry,
            )?;
            *self = Self::RequestRead;
            Ok(())
        }

        #[allow(clippy::too_many_arguments)]
        pub(crate) fn update<'p>(
            &mut self,
            received: ReceivedPdu<'p>,
            _header: PduHeader,
            maindevice: &MainDevice,
            retry_count: usize,
            timeout_duration: &Timespec,
            tx_entries: &mut BTreeMap<u64, TxBuf>,
            sock: &RawSocketDesc,
            ring: &mut IoUring,
            configured_addr: u16,
            index: u16,
            identifier: u8,
            write_entry: impl Fn(u64) -> u64,
            timeout_entry: impl Fn(u64) -> u64,
        ) -> Result<Option<ReceivedPdu<'p>>, Error> {
            match self {
                Self::RequestRead => {
                    let (frame, handle) = maindevice
                        .prep_wait_for_eeprom_chunk(configured_addr)?
                        .unwrap();

                    setup_write(
                        frame,
                        handle,
                        retry_count,
                        timeout_duration,
                        tx_entries,
                        sock,
                        ring,
                        Some(index),
                        Some(identifier),
                        &write_entry,
                        &timeout_entry,
                    )?;
                    *self = Self::WaitForDevice;
                }
                Self::WaitForDevice => {
                    use ethercrab::EtherCrabWireRead;
                    let ctrl = ethercrab::SiiControl::unpack_from_slice(&received)?;
                    let (frame, handle) = if ctrl.busy {
                        // resubmit the wait req
                        maindevice.prep_wait_for_eeprom_chunk(configured_addr)
                    } else {
                        *self = Self::ReadData;
                        // send a read req
                        maindevice.prep_read_available_eeprom_chunk(configured_addr, ctrl)
                    }?
                    .unwrap();

                    setup_write(
                        frame,
                        handle,
                        retry_count,
                        timeout_duration,
                        tx_entries,
                        sock,
                        ring,
                        Some(index),
                        Some(identifier),
                        &write_entry,
                        &timeout_entry,
                    )?;
                }
                Self::ReadData => return Ok(Some(received)),
            }
            Ok(None)
        }
    }
}

pub mod range {
    use super::*;
    pub struct RangeReader<const N: usize> {
        pub(crate) start: u16,
        pub(crate) offset: u16,
        pub(crate) len: u16,
        pub(crate) buffer: [u8; N],
        state: RegisterReadState,
        pub(crate) identifier: u8,
    }

    impl<const N: usize> RangeReader<N> {
        pub fn new(start: u16, len: u16, buffer: [u8; N], identifier: u8) -> Self {
            Self {
                start,
                len,
                buffer,
                state: RegisterReadState::RequestRead,
                offset: 0,
                identifier,
            }
        }

        #[allow(clippy::too_many_arguments)]
        pub fn start(
            &mut self,
            maindevice: &MainDevice,
            retry_count: usize,
            timeout_duration: &Timespec,
            tx_entries: &mut BTreeMap<u64, TxBuf>,
            sock: &RawSocketDesc,
            ring: &mut IoUring,
            configured_addr: u16,
            idx: u16,
            write_entry: impl Fn(u64) -> u64,
            timeout_entry: impl Fn(u64) -> u64,
        ) -> Result<(), Error> {
            self.state.start(
                maindevice,
                retry_count,
                timeout_duration,
                tx_entries,
                sock,
                ring,
                configured_addr,
                self.start + (self.offset / 2),
                idx,
                self.identifier,
                &write_entry,
                &timeout_entry,
            )
        }

        #[allow(clippy::too_many_arguments)]
        pub fn update(
            &mut self,
            received: ReceivedPdu<'_>,
            header: PduHeader,
            maindevice: &MainDevice,
            retry_count: usize,
            timeout_duration: &Timespec,
            tx_entries: &mut BTreeMap<u64, TxBuf>,
            sock: &RawSocketDesc,
            ring: &mut IoUring,
            configured_addr: u16,
            index: u16,
            write_entry: impl Fn(u64) -> u64,
            timeout_entry: impl Fn(u64) -> u64,
        ) -> Result<bool, Error> {
            if let Some(buf) = self.state.update(
                received,
                header,
                maindevice,
                retry_count,
                timeout_duration,
                tx_entries,
                sock,
                ring,
                configured_addr,
                index,
                self.identifier,
                &write_entry,
                &timeout_entry,
            )? {
                let bytes = &*buf;

                let skip = usize::from(self.offset % 2);
                let chunk = bytes.get(skip..).unwrap();

                let min_len = core::cmp::min(usize::from(self.len), self.buffer.len());
                let remaining_buf = &mut self.buffer
                    [usize::from(core::cmp::min(self.offset % (N as u16), self.len))..min_len];

                if remaining_buf.len() <= chunk.len() {
                    let chunk = &chunk[..remaining_buf.len()];
                    remaining_buf.copy_from_slice(chunk);
                    self.offset += remaining_buf.len() as u16;
                    return Ok(true);
                }

                self.offset += chunk.len() as u16;
                let buf = &mut remaining_buf[..chunk.len()];
                buf.copy_from_slice(chunk);
                // continue to read.
                self.start(
                    maindevice,
                    retry_count,
                    timeout_duration,
                    tx_entries,
                    sock,
                    ring,
                    configured_addr,
                    index,
                    &write_entry,
                    &timeout_entry,
                )?;
            }
            Ok(false)
        }
    }
}

pub mod category {
    use super::*;
    pub struct CategoryReader {
        pub(crate) found: Option<Found>,
        state: RegisterReadState,
        addr: u16,
        category: ethercrab::CategoryType,
        empty_category_count: u16,
        pub(crate) identifier: u8,
    }

    impl CategoryReader {
        pub fn new(category: ethercrab::CategoryType, identifier: u8) -> Self {
            Self {
                found: None,
                state: RegisterReadState::RequestRead,
                //addr: SII_FIRST_CATEGORY_START,
                addr: 0x0040u16,
                category,
                empty_category_count: 0,
                identifier,
            }
        }

        #[allow(clippy::too_many_arguments)]
        pub fn start(
            &mut self,
            maindevice: &MainDevice,
            retry_count: usize,
            timeout_duration: &Timespec,
            tx_entries: &mut BTreeMap<u64, TxBuf>,
            sock: &RawSocketDesc,
            ring: &mut IoUring,
            configured_addr: u16,
            idx: u16,
            write_entry: impl Fn(u64) -> u64,
            timeout_entry: impl Fn(u64) -> u64,
        ) -> Result<(), Error> {
            self.state.start(
                maindevice,
                retry_count,
                timeout_duration,
                tx_entries,
                sock,
                ring,
                configured_addr,
                self.addr,
                idx,
                self.identifier,
                &write_entry,
                &timeout_entry,
            )
        }

        #[allow(clippy::too_many_arguments)]
        pub fn update(
            &mut self,
            received: ReceivedPdu<'_>,
            header: PduHeader,
            maindevice: &MainDevice,
            retry_count: usize,
            timeout_duration: &Timespec,
            tx_entries: &mut BTreeMap<u64, TxBuf>,
            sock: &RawSocketDesc,
            ring: &mut IoUring,
            configured_addr: u16,
            index: u16,
            write_entry: impl Fn(u64) -> u64,
            timeout_entry: impl Fn(u64) -> u64,
        ) -> Result<bool, Error> {
            if let Some(buf) = self.state.update(
                received,
                header,
                maindevice,
                retry_count,
                timeout_duration,
                tx_entries,
                sock,
                ring,
                configured_addr,
                index,
                self.identifier,
                &write_entry,
                &timeout_entry,
            )? {
                let bytes = &*buf;

                if self.addr.checked_add(2).is_none() {
                    // could not find category or the end marker
                    self.found = None;
                    return Ok(true);
                }

                let (ty, len) = {
                    let (ty, rem) = bytes.split_first_chunk::<2>().unwrap();
                    let (len, _) = rem.split_first_chunk::<2>().unwrap();
                    (ty, len)
                };

                // from the category type and length words
                self.addr += 2;
                use ethercrab::CategoryType;

                let category_type = CategoryType::from(u16::from_le_bytes(*ty));
                let len_words = u16::from_le_bytes(*len);

                if len_words == 0 {
                    self.empty_category_count += 1;
                }

                if self.empty_category_count >= 32 {
                    // whole bunch of empty categories
                    return Ok(true);
                }

                match category_type {
                    cat if cat == self.category => {
                        self.found = Some(Found {
                            start: self.addr,
                            len: len_words * 2,
                        });
                        return Ok(true);
                    }
                    CategoryType::End => {
                        self.found = None;
                        return Ok(true);
                    }
                    _ => (),
                }

                self.addr += len_words;
                // did not find desired category, continue
                self.start(
                    maindevice,
                    retry_count,
                    timeout_duration,
                    tx_entries,
                    sock,
                    ring,
                    configured_addr,
                    index,
                    &write_entry,
                    &timeout_entry,
                )?;
            }
            Ok(false)
        }
    }

    pub enum CategoryIter<const N: usize> {
        Categories(CategoryReader),
        Category(RangeReader<N>, Found),
    }

    impl<const N: usize> CategoryIter<N> {
        pub fn new(category: ethercrab::CategoryType, identifier: u8) -> Self {
            Self::Categories(CategoryReader::new(category, identifier))
        }

        #[allow(clippy::too_many_arguments)]
        pub fn start(
            &mut self,
            maindevice: &MainDevice,
            retry_count: usize,
            timeout_duration: &Timespec,
            tx_entries: &mut BTreeMap<u64, TxBuf>,
            sock: &RawSocketDesc,
            ring: &mut IoUring,
            configured_addr: u16,
            idx: u16,
            write_entry: impl Fn(u64) -> u64,
            timeout_entry: impl Fn(u64) -> u64,
        ) -> Result<(), Error> {
            match self {
                Self::Categories(cat) => cat.start(
                    maindevice,
                    retry_count,
                    timeout_duration,
                    tx_entries,
                    sock,
                    ring,
                    configured_addr,
                    idx,
                    write_entry,
                    timeout_entry,
                ),
                _ => unreachable!(),
            }
        }

        // returns Some(..) if iteration is ready, Some(true) if more expected to follow, otherwise
        // Some(false)
        #[allow(clippy::too_many_arguments)]
        pub fn update(
            &mut self,
            received: ReceivedPdu<'_>,
            header: PduHeader,
            maindevice: &MainDevice,
            retry_count: usize,
            timeout_duration: &Timespec,
            tx_entries: &mut BTreeMap<u64, TxBuf>,
            sock: &RawSocketDesc,
            ring: &mut IoUring,
            configured_addr: u16,
            idx: u16,
            write_entry: impl Fn(u64) -> u64,
            timeout_entry: impl Fn(u64) -> u64,
        ) -> Result<Option<bool>, Error> {
            match self {
                Self::Categories(cat) => {
                    if cat.update(
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
                        &timeout_entry,
                    )? {
                        match core::mem::take(&mut cat.found) {
                            // could not find category :(
                            None => return Ok(Some(false)),
                            Some(found) => {
                                let mut reader =
                                    RangeReader::new(found.start, N as _, [0; N], cat.identifier);
                                reader.start(
                                    maindevice,
                                    retry_count,
                                    timeout_duration,
                                    tx_entries,
                                    sock,
                                    ring,
                                    configured_addr,
                                    idx,
                                    &write_entry,
                                    &timeout_entry,
                                )?;
                                *self = Self::Category(reader, found);
                            }
                        }
                    }
                }
                Self::Category(cat, found) => {
                    if cat.update(
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
                        &timeout_entry,
                    )? {
                        if cat.start + (cat.offset / 2) >= found.start + (found.len / 2) {
                            return Ok(Some(false));
                        }

                        cat.start(
                            maindevice,
                            retry_count,
                            timeout_duration,
                            tx_entries,
                            sock,
                            ring,
                            configured_addr,
                            idx,
                            &write_entry,
                            &timeout_entry,
                        )?;

                        return Ok(Some(true));
                    }
                }
            }
            Ok(None)
        }

        pub fn buffer(&self) -> Option<&[u8]> {
            match self {
                Self::Category(cat, _) => Some(&cat.buffer),
                _ => None,
            }
        }
    }
}

pub mod string {
    use super::*;
    pub struct StringReader {
        first: bool,
        string_offset: u8,
        offset: u16,
        start: u16,
        state: RegisterReadState,
        pub(crate) identifier: u8,
        pub(crate) found: Option<Found>,
    }

    impl StringReader {
        pub fn new(string_offset: u8, start: u16, identifier: u8) -> Self {
            Self {
                found: None,
                first: true,
                string_offset,
                offset: 0,
                start,
                state: RegisterReadState::RequestRead,
                identifier,
            }
        }

        #[allow(clippy::too_many_arguments)]
        pub fn start(
            &mut self,
            maindevice: &MainDevice,
            retry_count: usize,
            timeout_duration: &Timespec,
            tx_entries: &mut BTreeMap<u64, TxBuf>,
            sock: &RawSocketDesc,
            ring: &mut IoUring,
            configured_addr: u16,
            idx: u16,
            write_entry: impl Fn(u64) -> u64,
            timeout_entry: impl Fn(u64) -> u64,
        ) -> Result<(), Error> {
            self.state.start(
                maindevice,
                retry_count,
                timeout_duration,
                tx_entries,
                sock,
                ring,
                configured_addr,
                self.start + (self.offset / 2),
                idx,
                self.identifier,
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
            timeout_duration: &Timespec,
            tx_entries: &mut BTreeMap<u64, TxBuf>,
            sock: &RawSocketDesc,
            ring: &mut IoUring,
            configured_addr: u16,
            index: u16,
            write_entry: impl Fn(u64) -> u64,
            timeout_entry: impl Fn(u64) -> u64,
        ) -> Result<bool, Error> {
            if let Some(buf) = self.state.update(
                received,
                header,
                maindevice,
                retry_count,
                timeout_duration,
                tx_entries,
                sock,
                ring,
                configured_addr,
                index,
                self.identifier,
                &write_entry,
                &timeout_entry,
            )? {
                let bytes = &*buf;

                let skip = usize::from(self.offset % 2);
                let bytes = bytes.get(skip..).unwrap();

                let len = if self.first {
                    self.first = false;
                    let count = bytes[0];
                    if count < self.string_offset {
                        return Ok(true);
                    }
                    self.offset += 1;
                    bytes[1]
                } else {
                    bytes[0]
                };

                // since indices start from 1 in ethercat this is fine to do.
                self.string_offset -= 1;
                self.offset += 1;

                if self.string_offset == 0 {
                    // found string!
                    self.found = Some(Found {
                        start: self.start + self.offset - 1,
                        len: len as _,
                    });
                    return Ok(true);
                }
                self.offset += len as u16;

                // continue to read.
                self.start(
                    maindevice,
                    retry_count,
                    timeout_duration,
                    tx_entries,
                    sock,
                    ring,
                    configured_addr,
                    index,
                    &write_entry,
                    &timeout_entry,
                )?;
            }
            Ok(false)
        }
    }
}
