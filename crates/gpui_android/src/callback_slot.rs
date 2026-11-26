//! A re-entrancy-safe slot for a callback that may be replaced while it runs.

use std::cell::{Cell, RefCell};

pub(crate) struct CallbackSlot<T> {
    value: RefCell<Option<T>>,
    generation: Cell<u64>,
}

impl<T> Default for CallbackSlot<T> {
    fn default() -> Self {
        Self { value: RefCell::new(None), generation: Cell::new(0) }
    }
}

impl<T> CallbackSlot<T> {
    pub(crate) fn set(&self, value: T) {
        self.generation.set(self.generation.get().wrapping_add(1));
        drop(self.value.replace(Some(value)));
    }

    pub(crate) fn take(&self) -> Option<T> {
        self.generation.set(self.generation.get().wrapping_add(1));
        self.value.borrow_mut().take()
    }

    pub(crate) fn with<R>(&self, callback: impl FnOnce(&mut T) -> R) -> Option<R> {
        let mut value = self.value.borrow_mut().take()?;
        let generation = self.generation.get();
        let result = callback(&mut value);
        if self.generation.get() == generation {
            drop(self.value.replace(Some(value)));
        }
        Some(result)
    }
}

impl<T> CallbackSlot<T> {
    pub(crate) fn is_set(&self) -> bool {
        self.value.borrow().is_some()
    }
}
