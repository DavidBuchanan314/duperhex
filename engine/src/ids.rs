//! Typed indices: each kind of thing the pack defines is numbered by its own id type, and stored in
//! an `IdVec` that only that id type can index.

use std::marker::PhantomData;
use std::ops::{Index, IndexMut};

pub trait Id: Copy {
    fn new(i: usize) -> Self;
    fn index(self) -> usize;
}

macro_rules! id {
    ($($name:ident),*) => {$(
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
        pub struct $name(u16);

        impl Id for $name {
            fn new(i: usize) -> Self {
                $name(u16::try_from(i).expect("id out of range"))
            }

            fn index(self) -> usize {
                self.0 as usize
            }
        }
    )*};
}

id!(LevelId, PatternId, TrackId, SoundId, DirectorId, RotationId);

pub struct IdVec<I, T> {
    items: Vec<T>,
    id: PhantomData<I>,
}

impl<I: Id, T> IdVec<I, T> {
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.items.iter()
    }

    pub fn ids(&self) -> impl Iterator<Item = I> + use<I, T> {
        (0..self.items.len()).map(I::new)
    }

    pub fn iter_enumerated(&self) -> impl Iterator<Item = (I, &T)> {
        self.items.iter().enumerate().map(|(i, t)| (I::new(i), t))
    }

    pub fn position(&self, f: impl FnMut(&T) -> bool) -> Option<I> {
        self.items.iter().position(f).map(I::new)
    }
}

impl<I, T> From<Vec<T>> for IdVec<I, T> {
    fn from(items: Vec<T>) -> Self {
        IdVec { items, id: PhantomData }
    }
}

impl<I, T> FromIterator<T> for IdVec<I, T> {
    fn from_iter<It: IntoIterator<Item = T>>(it: It) -> Self {
        Vec::from_iter(it).into()
    }
}

impl<I: Id, T> Index<I> for IdVec<I, T> {
    type Output = T;

    fn index(&self, i: I) -> &T {
        &self.items[i.index()]
    }
}

impl<I: Id, T> IndexMut<I> for IdVec<I, T> {
    fn index_mut(&mut self, i: I) -> &mut T {
        &mut self.items[i.index()]
    }
}

impl<'a, I, T> IntoIterator for &'a IdVec<I, T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}
