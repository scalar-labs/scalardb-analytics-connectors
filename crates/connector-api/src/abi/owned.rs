//! Output structure trees handed to the C caller, and the memory they point to.
//!
//! An output, such as a table metadata result, is a tree of C structures in
//! the shape of an ordinary C API: the root points to arrays of structures,
//! which point to further structures and text. The library allocates the
//! tree and passes ownership of it to the caller, which becomes responsible
//! for freeing it: it reads the tree through the root pointer for as long as
//! it needs and then calls the free function named for the root, once.
//!
//! A C library would `malloc` each part and free them by walking the tree.
//! Here the structures themselves own nothing; every allocation their
//! pointers refer to is kept in an [`Arena`], which sits behind the root in
//! one [`Owned`] value. The free function drops that value, releasing every
//! allocation at once without knowing the tree's shape.

use std::{any::Any, ptr};

use super::text::{ScalardbAnalyticsConnectorString, TerminatedText};

/// Every allocation that one output tree's structures point to.
///
/// Building a tree stores each part here (text, arrays of structures, and
/// single structures) and writes the returned pointer into its parent. Each
/// part is its own heap allocation that is never moved or resized after it
/// is stored, so the pointer stays valid until the arena is dropped, even
/// when the arena itself moves.
#[derive(Default)]
pub(super) struct Arena(Vec<Box<dyn Any>>);

impl Arena {
    /// Stores text and returns its NUL-terminated view.
    pub(super) fn text(&mut self, value: &str) -> ScalardbAnalyticsConnectorString {
        let text = Box::new(TerminatedText::new(value));
        let view = text.view();
        self.0.push(text);
        view
    }

    /// Stores text when present; absent text has a null `data`.
    pub(super) fn optional_text(
        &mut self,
        value: Option<&str>,
    ) -> ScalardbAnalyticsConnectorString {
        value.map_or(ScalardbAnalyticsConnectorString::ABSENT, |value| {
            self.text(value)
        })
    }

    /// Stores views and returns a pointer to the first and their count.
    pub(super) fn array<T: 'static>(&mut self, items: Vec<T>) -> (*const T, usize) {
        if items.is_empty() {
            return (ptr::null(), 0);
        }
        let items = items.into_boxed_slice();
        let view = (items.as_ptr(), items.len());
        self.0.push(Box::new(items));
        view
    }

    /// Stores one view and returns a pointer to it.
    pub(super) fn one<T: 'static>(&mut self, item: T) -> *const T {
        let item = Box::new(item);
        let view: *const T = &*item;
        self.0.push(item);
        view
    }
}

/// An output tree as handed to the C caller: its root structure and the
/// arena holding everything the tree points to.
///
/// `view` is the first field, so a pointer to an `Owned<V>` is a pointer to
/// its `V`. The caller sees only a pointer to the root structure, as declared
/// in the header; the arena behind it is invisible to C and stays alive
/// until the caller frees the root.
#[repr(C)]
pub(super) struct Owned<V> {
    view: V,
    _arena: Arena,
}

impl<V> Owned<V> {
    /// Moves the output to the heap and passes it to the caller: Rust no
    /// longer drops it, and the returned pointer is the caller's until it
    /// calls [`Self::free`] through the root's free function.
    pub(super) fn into_raw(view: V, arena: Arena) -> *mut V {
        Box::into_raw(Box::new(Self {
            view,
            _arena: arena,
        }))
        .cast()
    }

    /// Takes an output back from the caller and drops it, with every
    /// allocation of its tree.
    ///
    /// # Safety
    ///
    /// `view` is null or was returned by `Owned::<V>::into_raw` and is not
    /// used after this call.
    pub(super) unsafe fn free(view: *mut V) {
        if !view.is_null() {
            // SAFETY: forwarded from the caller; the pointer came from a boxed
            // `Owned<V>` whose first field is the view.
            drop(unsafe { Box::from_raw(view.cast::<Self>()) });
        }
    }
}
