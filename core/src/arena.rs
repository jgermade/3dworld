//! Generational arena. Handles are `u32` indices, not pointers.
//!
//! This is a rule rather than a taste — see `AGENTS.md`. Heavy work is meant to
//! run in workers with their *own* linear memory, where a pointer is
//! meaningless one line later and an index is still valid. It also halves the
//! size of every node against 64-bit pointers, and it is what undo and
//! serialisation want anyway.
//!
//! The generation is what makes a stale handle an error instead of a
//! misdirection. Without it, deleting a node and adding another hands the new
//! one the old one's identity, and every reference held elsewhere — a
//! selection, a history entry — silently starts naming the wrong thing.
//!
//! **Slots are never reused, and that is not an oversight.** A free list was
//! written first and removed the same afternoon: undo restores a node into the
//! slot it came from, so a free list means a later insert can take the slot an
//! undo still needs, and the undo then silently does nothing. The bug is in the
//! 2026-08-25 session file under `Bugs found by building, not by reading`,
//! because it is exactly the kind that gets reintroduced by someone optimising
//! memory in good faith.
//!
//! The price is that an arena grows with the total number of nodes a session
//! ever created, not the number alive. Compaction is possible — but only while
//! nothing can restore, which means only with history cleared, and it renumbers
//! every id. It is not implemented; [`Arena::slot_count`] is how you see the
//! cost.

use core::hash::{Hash, Hasher};
use core::marker::PhantomData;

pub struct Id<T> {
    index: u32,
    generation: u32,
    // `fn() -> T` so that `Id<T>` is Send + Sync + Copy whatever `T` is: a
    // handle is a number, and it should travel wherever a number can.
    marker: PhantomData<fn() -> T>,
}

impl<T> Id<T> {
    fn new(index: u32, generation: u32) -> Self {
        Self {
            index,
            generation,
            marker: PhantomData,
        }
    }

    pub fn index(self) -> u32 {
        self.index
    }

    pub fn generation(self) -> u32 {
        self.generation
    }
}

impl<T> Clone for Id<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Id<T> {}

impl<T> PartialEq for Id<T> {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index && self.generation == other.generation
    }
}

impl<T> Eq for Id<T> {}

impl<T> Hash for Id<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.index.hash(state);
        self.generation.hash(state);
    }
}

/// Ordered by index then generation, so that anything iterating a set of ids
/// does so in the same order on every machine. Determinism is a product
/// property here, not a convenience.
impl<T> PartialOrd for Id<T> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for Id<T> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        (self.index, self.generation).cmp(&(other.index, other.generation))
    }
}

impl<T> core::fmt::Debug for Id<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "#{}v{}", self.index, self.generation)
    }
}

struct Slot<T> {
    generation: u32,
    value: Option<T>,
}

pub struct Arena<T> {
    slots: Vec<Slot<T>>,
}

impl<T> Default for Arena<T> {
    fn default() -> Self {
        Self { slots: Vec::new() }
    }
}

impl<T> Arena<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.slots.iter().filter(|s| s.value.is_some()).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many slots exist, alive or not. Always at least [`Arena::len`], and
    /// the gap between them is what a session has spent on deleted nodes.
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    pub fn insert(&mut self, value: T) -> Id<T> {
        let index = self.slots.len() as u32;
        self.slots.push(Slot {
            generation: 0,
            value: Some(value),
        });
        Id::new(index, 0)
    }

    fn slot(&self, id: Id<T>) -> Option<&Slot<T>> {
        self.slots
            .get(id.index as usize)
            .filter(|s| s.generation == id.generation)
    }

    pub fn get(&self, id: Id<T>) -> Option<&T> {
        self.slot(id)?.value.as_ref()
    }

    pub fn get_mut(&mut self, id: Id<T>) -> Option<&mut T> {
        self.slots
            .get_mut(id.index as usize)
            .filter(|s| s.generation == id.generation)?
            .value
            .as_mut()
    }

    pub fn contains(&self, id: Id<T>) -> bool {
        self.get(id).is_some()
    }

    pub fn remove(&mut self, id: Id<T>) -> Option<T> {
        let slot = self
            .slots
            .get_mut(id.index as usize)
            .filter(|s| s.generation == id.generation)?;
        let value = slot.value.take()?;
        // Bumping is what invalidates every outstanding copy of `id`. The slot
        // itself stays where it is, reserved for an undo that may want it back.
        slot.generation = slot.generation.wrapping_add(1);
        Some(value)
    }

    /// Put a value back under an id that [`Arena::remove`] previously
    /// invalidated, restoring its generation.
    ///
    /// This is undo, and nothing else. It is the one operation that may rewind
    /// a generation, which is precisely what makes stale handles unsafe again
    /// — so it is deliberately not part of the general API's shape: it fails
    /// unless the slot is empty and its generation is exactly one ahead.
    /// History is the only caller, and because slots are never reused those
    /// two conditions hold for as long as history keeps the entry.
    pub(crate) fn restore(&mut self, id: Id<T>, value: T) -> bool {
        let Some(slot) = self.slots.get_mut(id.index as usize) else {
            return false;
        };
        if slot.value.is_some() || slot.generation != id.generation.wrapping_add(1) {
            return false;
        }
        slot.generation = id.generation;
        slot.value = Some(value);
        true
    }

    pub fn iter(&self) -> impl Iterator<Item = (Id<T>, &T)> {
        self.slots.iter().enumerate().filter_map(|(i, slot)| {
            slot.value
                .as_ref()
                .map(|v| (Id::new(i as u32, slot.generation), v))
        })
    }

    /// Compact the arena by removing empty (tombstone) slots and re-indexing live items.
    ///
    /// Returns a map from `old_id -> new_id` for all surviving items so callers can
    /// update external references (e.g. selection sets).
    ///
    /// # Invariant
    /// Must only be invoked when history/undo is empty, because re-indexing alters
    /// slot indices which breaks past undo handles.
    pub fn compact(&mut self) -> std::collections::BTreeMap<Id<T>, Id<T>> {
        let mut new_slots = Vec::with_capacity(self.len());
        let mut id_map = std::collections::BTreeMap::new();

        for (old_idx, slot) in self.slots.drain(..).enumerate() {
            if let Some(value) = slot.value {
                let new_idx = new_slots.len() as u32;
                let old_id = Id::new(old_idx as u32, slot.generation);
                let new_id = Id::new(new_idx, slot.generation);
                new_slots.push(Slot {
                    generation: slot.generation,
                    value: Some(value),
                });
                id_map.insert(old_id, new_id);
            }
        }
        self.slots = new_slots;
        id_map
    }
}
