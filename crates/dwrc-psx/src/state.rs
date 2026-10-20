//! Save states: the whole console written to bytes and read back.
//!
//! Every device lists its fields once, in a `sync` function that either
//! writes them ([`Sync::save`]) or reads them back ([`Sync::load`]), so saving
//! and loading can never disagree about the layout. What is not part of the
//! console's state is left out on purpose: the BIOS ROM and the disc (the
//! same ones must be in use when a state is loaded; the caller checks), code
//! hooks (they belong to the tools and mods running now), memory card
//! contents (loading a state never takes a player's saves back) and output
//! that has already been handed to the host (audio samples, debug text).

/// Writes or reads console state.
pub struct Sync<'a> {
    out: Option<&'a mut Vec<u8>>,
    input: &'a [u8],
    pos: usize,
    error: bool,
}

impl<'a> Sync<'a> {
    pub fn save(out: &'a mut Vec<u8>) -> Self {
        Self { out: Some(out), input: &[], pos: 0, error: false }
    }

    pub fn load(input: &'a [u8]) -> Self {
        Self { out: None, input, pos: 0, error: false }
    }

    pub fn loading(&self) -> bool {
        self.out.is_none()
    }

    /// True when a load ran out of data (a truncated or foreign state).
    pub fn failed(&self) -> bool {
        self.error
    }

    /// All the input was read (loading) and nothing went wrong.
    pub fn finished(&self) -> bool {
        !self.error && (self.out.is_some() || self.pos == self.input.len())
    }

    fn bytes<const N: usize>(&mut self, v: [u8; N]) -> [u8; N] {
        match &mut self.out {
            Some(o) => {
                o.extend_from_slice(&v);
                v
            }
            None => match self.input.get(self.pos..self.pos + N) {
                Some(b) => {
                    self.pos += N;
                    b.try_into().unwrap()
                }
                None => {
                    self.error = true;
                    v
                }
            },
        }
    }

    /// Syncs a length (of a vector or queue) and returns it.
    fn len(&mut self, n: usize) -> usize {
        let mut v = n as u32;
        v.sync(self);
        if self.loading() && v as usize > self.input.len().saturating_sub(self.pos) {
            // More elements than bytes left (each takes at least one): corrupt input.
            self.error = true;
            return 0;
        }
        v as usize
    }
}

/// A value that is part of the console state.
pub trait Field {
    fn sync(&mut self, s: &mut Sync);
}

macro_rules! numbers {
    ($($t:ty),*) => {$(
        impl Field for $t {
            #[inline]
            fn sync(&mut self, s: &mut Sync) {
                *self = <$t>::from_le_bytes(s.bytes(self.to_le_bytes()));
            }
        }
    )*};
}
numbers!(u8, i8, u16, i16, u32, i32, u64, i64, f32);

impl Field for bool {
    fn sync(&mut self, s: &mut Sync) {
        let mut b = *self as u8;
        b.sync(s);
        *self = b != 0;
    }
}

impl Field for usize {
    fn sync(&mut self, s: &mut Sync) {
        let mut v = *self as u64;
        v.sync(s);
        *self = v as usize;
    }
}

impl<T: Field, const N: usize> Field for [T; N] {
    fn sync(&mut self, s: &mut Sync) {
        for x in self.iter_mut() {
            x.sync(s);
        }
    }
}

impl Field for [u8] {
    fn sync(&mut self, s: &mut Sync) {
        match &mut s.out {
            Some(o) => o.extend_from_slice(self),
            None => match s.input.get(s.pos..s.pos + self.len()) {
                Some(b) => {
                    self.copy_from_slice(b);
                    s.pos += self.len();
                }
                None => s.error = true,
            },
        }
    }
}

impl<T: Field, const N: usize> Field for Box<[T; N]> {
    fn sync(&mut self, s: &mut Sync) {
        (**self).sync(s);
    }
}

/// Fixed-size buffers (RAM, VRAM): the size is part of the console, so a
/// state with another size is rejected rather than resized.
impl<T: Field> Field for Box<[T]> {
    fn sync(&mut self, s: &mut Sync) {
        let n = s.len(self.len());
        if n != self.len() {
            s.error = true;
            return;
        }
        for x in self.iter_mut() {
            x.sync(s);
        }
    }
}

impl<T: Field + Default> Field for Vec<T> {
    fn sync(&mut self, s: &mut Sync) {
        let n = s.len(self.len());
        if s.loading() {
            self.clear();
            self.resize_with(n, T::default);
        }
        for x in self.iter_mut() {
            x.sync(s);
        }
    }
}

impl<T: Field + Default> Field for std::collections::VecDeque<T> {
    fn sync(&mut self, s: &mut Sync) {
        let n = s.len(self.len());
        if s.loading() {
            self.clear();
            self.resize_with(n, T::default);
        }
        for x in self.iter_mut() {
            x.sync(s);
        }
    }
}

impl<T: Field + Default> Field for Option<T> {
    fn sync(&mut self, s: &mut Sync) {
        let mut some = self.is_some();
        some.sync(s);
        if s.loading() {
            *self = if some { Some(T::default()) } else { None };
        }
        if let Some(v) = self {
            v.sync(s);
        }
    }
}

impl<A: Field, B: Field> Field for (A, B) {
    fn sync(&mut self, s: &mut Sync) {
        self.0.sync(s);
        self.1.sync(s);
    }
}

/// Lists the fields of a struct that make up its state.
#[macro_export]
macro_rules! sync_fields {
    ($self:ident, $s:ident; $($f:ident),* $(,)?) => {
        $( $crate::state::Field::sync(&mut $self.$f, $s); )*
    };
}

/// A C-like enum synced as its discriminant.
#[macro_export]
macro_rules! sync_enum {
    ($t:ty { $($v:ident = $n:expr),* $(,)? }) => {
        impl $crate::state::Field for $t {
            fn sync(&mut self, s: &mut $crate::state::Sync) {
                let mut n: u8 = match self { $(Self::$v => $n),* };
                n.sync(s);
                if s.loading() {
                    *self = match n { $($n => Self::$v,)* _ => *self };
                }
            }
        }
    };
}
