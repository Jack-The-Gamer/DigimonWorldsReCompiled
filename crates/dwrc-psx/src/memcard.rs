//! Memory card (128 KiB, 1024 sectors of 128 bytes) protocol.
//! Card images use the raw `.mcd` format shared with other emulators.

pub const CARD_SIZE: usize = 128 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Command,
    Read(u8),
    Write(u8),
    Id(u8),
}

#[derive(Clone)]
pub struct MemoryCard {
    pub data: Box<[u8]>,
    pub dirty: bool,
    flag: u8,
    state: State,
    sector: u16,
    msb: u8,
    index: usize,
    checksum: u8,
    write_buf: [u8; 128],
    last: u8,
}

impl MemoryCard {
    /// A freshly formatted card.
    pub fn new_formatted() -> Self {
        let mut data = vec![0u8; CARD_SIZE].into_boxed_slice();
        // Header frame
        data[0] = b'M';
        data[1] = b'C';
        data[127] = checksum(&data[0..127]);
        // Directory frames 1..15: free blocks
        for i in 1..16 {
            let f = &mut data[i * 128..(i + 1) * 128];
            f[0] = 0xA0;
            f[8] = 0xFF;
            f[9] = 0xFF;
            f[127] = checksum(&f[0..127]);
        }
        // Broken sector list frames 16..35
        for i in 16..36 {
            let f = &mut data[i * 128..(i + 1) * 128];
            f[0..4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
            f[8] = 0xFF;
            f[9] = 0xFF;
            f[127] = checksum(&f[0..127]);
        }
        // Frame 63: write test frame (copy of header)
        let hdr: Vec<u8> = data[0..128].to_vec();
        data[63 * 128..64 * 128].copy_from_slice(&hdr);
        Self::from_bytes(data)
    }

    pub fn from_bytes(data: Box<[u8]>) -> Self {
        assert_eq!(data.len(), CARD_SIZE);
        Self {
            data,
            dirty: false,
            flag: 0x08,
            state: State::Idle,
            sector: 0,
            msb: 0,
            index: 0,
            checksum: 0,
            write_buf: [0; 128],
            last: 0,
        }
    }

    pub fn reset_protocol(&mut self) {
        self.state = State::Idle;
    }

    /// Exchanges one byte; returns (response, ack).
    pub fn transfer(&mut self, v: u8) -> (u8, bool) {
        match self.state {
            State::Idle => {
                if v == 0x81 {
                    self.state = State::Command;
                    (0xFF, true)
                } else {
                    (0xFF, false)
                }
            }
            State::Command => match v {
                0x52 => {
                    self.state = State::Read(0);
                    (self.flag, true)
                }
                0x57 => {
                    self.state = State::Write(0);
                    (self.flag, true)
                }
                0x53 => {
                    self.state = State::Id(0);
                    (self.flag, true)
                }
                _ => {
                    self.state = State::Idle;
                    (self.flag, false)
                }
            },
            State::Read(step) => {
                let (r, ack) = match step {
                    0 => (0x5A, true),
                    1 => (0x5D, true),
                    2 => {
                        self.msb = v;
                        (0x00, true)
                    }
                    3 => {
                        self.sector = ((self.msb as u16) << 8) | v as u16;
                        self.checksum = self.msb ^ v;
                        self.index = 0;
                        (self.msb, true)
                    }
                    4 => (0x5C, true),
                    5 => (0x5D, true),
                    6 => {
                        if self.sector > 0x3FF {
                            self.state = State::Idle;
                            return (0xFF, false);
                        }
                        ((self.sector >> 8) as u8, true)
                    }
                    7 => (self.sector as u8, true),
                    8 => {
                        let b = self.data[self.sector as usize * 128 + self.index];
                        self.checksum ^= b;
                        self.index += 1;
                        if self.index < 128 {
                            self.state = State::Read(8);
                            return (b, true);
                        }
                        (b, true)
                    }
                    9 => (self.checksum, true),
                    10 => {
                        self.state = State::Idle;
                        return (0x47, false);
                    }
                    _ => {
                        self.state = State::Idle;
                        return (0xFF, false);
                    }
                };
                if let State::Read(s) = self.state {
                    self.state = State::Read(s + 1);
                }
                (r, ack)
            }
            State::Write(step) => {
                let (r, ack) = match step {
                    0 => (0x5A, true),
                    1 => (0x5D, true),
                    2 => {
                        self.msb = v;
                        (0x00, true)
                    }
                    3 => {
                        self.sector = ((self.msb as u16) << 8) | v as u16;
                        self.checksum = self.msb ^ v;
                        self.index = 0;
                        self.last = self.msb;
                        (self.msb, true)
                    }
                    4 => {
                        self.write_buf[self.index] = v;
                        self.checksum ^= v;
                        self.index += 1;
                        let r = self.last;
                        self.last = v;
                        if self.index < 128 {
                            self.state = State::Write(4);
                            return (r, true);
                        }
                        (r, true)
                    }
                    5 => {
                        let ok = v == self.checksum;
                        self.last = if ok { 0x47 } else { 0x4E };
                        if ok && self.sector <= 0x3FF {
                            let off = self.sector as usize * 128;
                            self.data[off..off + 128].copy_from_slice(&self.write_buf);
                            self.dirty = true;
                            self.flag &= !0x08;
                        }
                        if self.sector > 0x3FF {
                            self.last = 0xFF;
                        }
                        (self.write_buf[127], true)
                    }
                    6 => (0x5C, true),
                    7 => (0x5D, true),
                    8 => {
                        self.state = State::Idle;
                        return (self.last, false);
                    }
                    _ => {
                        self.state = State::Idle;
                        return (0xFF, false);
                    }
                };
                if let State::Write(s) = self.state {
                    self.state = State::Write(s + 1);
                }
                (r, ack)
            }
            State::Id(step) => {
                const ID: [u8; 8] = [0x5A, 0x5D, 0x5C, 0x5D, 0x04, 0x00, 0x00, 0x80];
                let i = step as usize;
                if i >= ID.len() - 1 {
                    self.state = State::Idle;
                    return (ID[ID.len() - 1], false);
                }
                self.state = State::Id(step + 1);
                (ID[i], true)
            }
        }
    }
}

fn checksum(b: &[u8]) -> u8 {
    b.iter().fold(0, |a, x| a ^ x)
}
