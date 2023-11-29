use core::{
    alloc::Layout,
    mem,
    num::NonZeroUsize,
    ptr::{self, NonNull},
};

use alloc::alloc::handle_alloc_error;
use hashbrown::{HashMap, HashSet};
use unique_type_id::UniqueTypeId;

use super::{bus, ComponentId};

fn any_as_bytes<T: Sized>(any: &T) -> &[u8] {
    // Safety: Since we are only reading data, this can't leave the struct in a bad state.
    unsafe {
        core::slice::from_raw_parts((any as *const T) as *const u8, core::mem::size_of::<T>())
    }
}

mod tuples;
pub use tuples::*;

/// # Safety
/// A struct will be constructed from the provided bytes. Those original bytes should be considered "uninitalized" after this,
/// because if a second instance of the struct was constructed from the bytes, it could result in it being dropped twice, and
/// that could result in a double free.
unsafe fn any_from_bytes<T: Sized>(bytes: &[u8]) -> T {
    let pointer = bytes as *const _ as *const T;

    unsafe { ptr::read(pointer) }
}

#[derive(Debug, Hash, Eq, PartialEq, Clone, Copy)]
pub struct RowIndex(bus::Width);

struct ColumnAllocation {
    data: NonNull<u8>,
    layout: Layout,
}

pub struct Column {
    // TODO we want to support using a per-component allocator.
    allocation: Option<ColumnAllocation>,
    element_layout: Layout,
    deconstructor: &'static dyn Fn(&[u8]),
}

impl Column {
    pub fn new<T: 'static>() -> Self {
        fn deconstructor<T>(data: &[u8]) {
            let pointer = data as *const _ as *const T;

            // Safety: We should only ever be dropping values that have not already been dropped.
            let value = unsafe { ptr::read(pointer) };
            drop(value);
        }

        let element_layout = Layout::new::<T>();
        let deconstructor = &deconstructor::<T>;

        Self {
            allocation: None,
            element_layout,
            deconstructor,
        }
    }

    fn inherit_type(other: &Self) -> Self {
        Self {
            allocation: None,
            element_layout: other.element_layout,
            deconstructor: other.deconstructor,
        }
    }

    /// # Safety
    /// You must not read from rows that have been dropped or have not yet been initalized.
    unsafe fn read_element(&self, index: RowIndex) -> &[u8] {
        let allocation = self.allocation.as_ref().unwrap();
        let element_length = self.element_layout.size();
        let pointer = allocation
            .data
            .as_ptr()
            .add(element_length * index.0 as usize);
        core::slice::from_raw_parts(pointer, element_length)
    }

    /// # Safety
    /// Must be in range of the column rows.
    unsafe fn set_element(&mut self, index: RowIndex, element: &[u8]) {
        let allocation = self.allocation.as_mut().unwrap();
        let element_length = self.element_layout.size();
        let pointer = allocation
            .data
            .as_ptr()
            .add(element_length * index.0 as usize);
        let slice = core::slice::from_raw_parts_mut(pointer, element_length);

        slice.copy_from_slice(element);
    }

    /// # Safety
    /// Must be in range of the column rows.
    /// You must not read from rows that have been dropped or have not yet been initalized.
    unsafe fn access_element_mut(&mut self, index: RowIndex) -> &mut [u8] {
        let allocation = self.allocation.as_mut().unwrap();
        let element_length = self.element_layout.size();
        let pointer = allocation
            .data
            .as_ptr()
            .add(element_length * index.0 as usize);
        core::slice::from_raw_parts_mut(pointer, element_length)
    }

    fn resize(&mut self, new_size: NonZeroUsize) {
        let element_length = self.element_layout.size();

        if element_length > 0 {
            let new_layout = array_layout(&self.element_layout, new_size.get())
                .expect("Failed to calculate new layout.");

            let new_data = if let Some(allocation) = self.allocation.as_mut() {
                // This is a re-allocation.
                // Safety:
                // Old pointer was allocated with this allocator (see below)
                // Layout of pointer was created from the same layout.
                // The original layout was non zero and the `new_size` is NonZeroUsize, so the size will not be zero.
                // `array_layout` made sure the layout didn't do an integer overflow.
                unsafe {
                    alloc::alloc::realloc(
                        allocation.data.as_ptr(),
                        allocation.layout,
                        new_layout.size(),
                    )
                }
            } else {
                // First time allocation, the current pointer is invalid.
                // Safety: We verified the layout has a length greater than zero.
                unsafe { alloc::alloc::alloc(new_layout) }
            };

            let data = NonNull::new(new_data).unwrap_or_else(|| handle_alloc_error(new_layout));
            self.allocation = Some(ColumnAllocation {
                data,
                layout: new_layout,
            });
        } else {
            // This component doesn't need memory to store. Don't allocate anything.
        }
    }

    /// Calls `drop()` on the contents of a row.
    /// Note that this does not free the memory from the column. We will reuse it.
    /// # Safety
    /// Row must be allocated and initalized before dropping.
    /// When a row is dropped, it is no longer valid. You must make sure not to read from it
    /// until a new value has been set.
    unsafe fn drop_row(&mut self, row: RowIndex) {
        let data = self.read_element(row);
        (self.deconstructor)(data)
    }
}

impl Drop for Column {
    fn drop(&mut self) {
        if let Some(allocation) = self.allocation.take() {
            // Safety: Data was allocated with the same allocator.
            // Data would still be set to None if the allocation had not happened.
            unsafe { alloc::alloc::dealloc(allocation.data.as_ptr(), allocation.layout) };
        }
    }
}

/// From <https://doc.rust-lang.org/beta/src/core/alloc/layout.rs.html>
fn array_layout(layout: &Layout, n: usize) -> Option<Layout> {
    let (array_layout, offset) = repeat_layout(layout, n)?;
    debug_assert_eq!(layout.size(), offset);
    Some(array_layout)
}

// TODO: replace with `Layout::repeat` if/when it stabilizes
/// From <https://doc.rust-lang.org/beta/src/core/alloc/layout.rs.html>
fn repeat_layout(layout: &Layout, n: usize) -> Option<(Layout, usize)> {
    // This cannot overflow. Quoting from the invariant of Layout:
    // > `size`, when rounded up to the nearest multiple of `align`,
    // > must not overflow (i.e., the rounded value must be less than
    // > `usize::MAX`)
    let padded_size = layout.size() + padding_needed_for(layout, layout.align());
    let alloc_size = padded_size.checked_mul(n)?;

    // SAFETY: self.align is already known to be valid and alloc_size has been
    // padded already.
    unsafe {
        Some((
            Layout::from_size_align_unchecked(alloc_size, layout.align()),
            padded_size,
        ))
    }
}

/// From <https://doc.rust-lang.org/beta/src/core/alloc/layout.rs.html>
const fn padding_needed_for(layout: &Layout, align: usize) -> usize {
    let len = layout.size();

    // Rounded up value is:
    //   len_rounded_up = (len + align - 1) & !(align - 1);
    // and then we return the padding difference: `len_rounded_up - len`.
    //
    // We use modular arithmetic throughout:
    //
    // 1. align is guaranteed to be > 0, so align - 1 is always
    //    valid.
    //
    // 2. `len + align - 1` can overflow by at most `align - 1`,
    //    so the &-mask with `!(align - 1)` will ensure that in the
    //    case of overflow, `len_rounded_up` will itself be 0.
    //    Thus the returned padding, when added to `len`, yields 0,
    //    which trivially satisfies the alignment `align`.
    //
    // (Of course, attempts to allocate blocks of memory whose
    // size and padding overflow in the above manner should cause
    // the allocator to yield an error anyway.)

    let len_rounded_up = len.wrapping_add(align).wrapping_sub(1) & !align.wrapping_sub(1);
    len_rounded_up.wrapping_sub(len)
}

const INITIAL_ROW_ALLOCATION: usize = 4;

pub struct ComponentStorage {
    // TODO the column should be able to allocate on different heaps.
    components: HashMap<ComponentId, Column>,
    free_rows: HashSet<RowIndex>,
    next_row: RowIndex,
    allocated_rows: usize,
}

impl ComponentStorage {
    pub fn empty_type() -> Self {
        Self {
            components: HashMap::new(),
            free_rows: HashSet::new(),
            next_row: RowIndex(0),
            allocated_rows: 0,
        }
    }

    /// Create a new storage that can store the components of this one but with storage for component T added.
    pub fn extend(&self, component_id: ComponentId, column: Column) -> Self {
        Self {
            components: self
                .components
                .iter()
                .map(|(component_id, column)| (*component_id, Column::inherit_type(column)))
                .chain(core::iter::once((component_id, column)))
                .collect(),
            free_rows: HashSet::new(),
            next_row: RowIndex(0),
            allocated_rows: 0,
        }
    }

    /// Create a new storage that can store the components of this one but with storage for component T removed.
    pub fn reduce(&self, component_id: ComponentId) -> Self {
        Self {
            components: self
                .components
                .iter()
                .filter(|(column_component_id, _column)| **column_component_id != component_id)
                .map(|(component_id, column)| (*component_id, Column::inherit_type(column)))
                .collect(),
            free_rows: HashSet::new(),
            next_row: RowIndex(0),
            allocated_rows: 0,
        }
    }

    pub fn drop_row(&mut self, row: RowIndex) {
        if self.free_rows.insert(row) {
            for column in self.components.values_mut() {
                // Safety: By using the `free_rows` hash set, we have verified that this
                // row has not been dropped.
                unsafe { column.drop_row(row) };
            }
        }
    }

    pub fn insert_row<I>(&mut self, to_insert: I) -> RowIndex
    where
        I: StorageInsert,
    {
        // Safety: We must make sure that the data we are inserting cannot be dropped.
        // We will forget the to_insert later to enforce that.
        let row = unsafe { self.insert_row_raw(to_insert.get_values()) };

        // Make sure that data doesn't get dropped, because we took ownership of it.
        mem::forget(to_insert);

        row
    }

    /// You must provide all components for all coumns. Missing any will leave the
    /// leave some columns short.
    /// # Safety
    /// You must make sure the value you are copying bytes from do not get dropped after insertion.
    unsafe fn insert_row_raw<'a>(
        &mut self,
        components: impl IntoIterator<Item = (ComponentId, &'a [u8])>,
    ) -> RowIndex {
        let components = components.into_iter();

        if let Some(next_row) = self.free_rows.iter().next().copied() {
            // We are going to reuse a row.
            self.free_rows.remove(&next_row);

            for (component_id, data) in components {
                let column = self.components.get_mut(&component_id).expect(
                    "Attempt to insert row for component that does not exist in arche type.",
                );

                // Safety: We checked earlier that this row is a free one, so we shouldn't be stomping over
                // any old data. Plus, we never read this, so it doesn't matter that it's uninitalized.
                // Since we are reusing an old row we are reusing, this means it is already allocated within the column.
                unsafe { column.set_element(next_row, data) }
            }

            next_row
        } else {
            // We must insert a new row.
            let next_row = self.next_row;
            self.next_row.0 += 1;

            // We may need to allocate more memory.
            let need_to_allocate = next_row.0 as usize >= self.allocated_rows;

            if need_to_allocate {
                // Double the memory we allocated, or start with the inital allocation.
                self.allocated_rows = INITIAL_ROW_ALLOCATION.max(self.allocated_rows * 2);
            }

            let mut num_components = 0;

            // Insert the elements.
            for (component_id, data) in components {
                let column = self.components.get_mut(&component_id).expect(
                    "Attempt to insert row for component that does not exist in arche type.",
                );

                if need_to_allocate {
                    column.resize(NonZeroUsize::new(self.allocated_rows).unwrap());
                }

                // Safety: The if statement above made sure enough memory has been allocated into each column.
                unsafe { column.set_element(next_row, data) }
                num_components += 1;
            }

            assert_eq!(
                num_components,
                self.components.len(),
                "Not enough components provided for arche type."
            );

            next_row
        }
    }

    pub fn extend_row_to_other<T: StorageInsert>(
        &mut self,
        other: &mut Self,
        row: RowIndex,
        new_components: T,
    ) -> RowIndex {
        // We need to make sure this value doesn't drop! Otherwise it could be invalidated when we restore it.
        let component_sources = self.iter_row(row).chain(new_components.get_values());

        // Safety: We make sure to forget the value later, so it won't be dropped.
        let new_row = unsafe { other.insert_row_raw(component_sources) };

        self.drop_row(row);
        core::mem::forget(new_components);

        new_row
    }

    pub fn reduce_row_to_other<T: ReduceArgument + ComponentSet>(
        &mut self,
        other: &mut Self,
        row: RowIndex,
    ) -> (RowIndex, T) {
        let component_sources = self
            .iter_row(row)
            .filter(|(component_id, _data)| !T::contains_component(*component_id));

        // Safety: The only value that will actually be dropped is the removed one, and we used a filter
        // above to make sure it doesn't get transferred to the new storage.
        let new_row = unsafe { other.insert_row_raw(component_sources) };

        // Safety: We have not transferred out the components that this will be building from. This is taking ownership of them.
        let removed_components = unsafe { T::build(self, row) };

        self.drop_row(row);

        (new_row, removed_components)
    }

    fn is_row_initalized(&self, row: RowIndex) -> bool {
        row.0 < self.next_row.0 || !self.free_rows.contains(&row)
    }

    fn access_row_raw<const N: usize>(
        &mut self,
        row: RowIndex,
        components: [ComponentId; N],
    ) -> [&mut [u8]; N] {
        // Check that the row is in the allocated range and not freed.
        if self.is_row_initalized(row) {
            let mut component_references_iter = components.iter();

            let component_references: [&ComponentId; N] =
                core::array::from_fn(|_i| component_references_iter.next().unwrap());

            // Prevents looking up components that do not exist or getting duplicate access to components.
            let columns = self.components.get_many_mut(component_references).expect("Requested component that does not exist in arche type or request contained duplicate component.");
            let mut columns = columns.into_iter();

            core::array::from_fn(|_i| {
                let column = columns.next().unwrap();
                // Safety: We checked that the row is within the column and is initalized up above.
                unsafe { column.access_element_mut(row) }
            })
        } else {
            panic!("Row is not allocated.")
        }
    }

    pub fn access_row<'a, A>(&'a mut self, row: RowIndex) -> A
    where
        A: StorageAccessor<'a>,
    {
        A::access(self, row)
    }

    pub fn iter_component_ids(&self) -> impl Iterator<Item = ComponentId> + '_ {
        self.components.keys().copied()
    }

    pub fn contains_component<T>(&self) -> bool
    where
        T: UniqueTypeId<bus::Width>,
    {
        self.components.contains_key(&T::id())
    }

    pub fn iter_row(&mut self, row: RowIndex) -> impl Iterator<Item = (ComponentId, &[u8])> {
        if self.is_row_initalized(row) {
            self.components
                .iter_mut()
                .map(move |(component_id, column)| {
                    (
                        *component_id,
                        // Safety: We check earlier that this is an initalized row.
                        unsafe { column.read_element(row) },
                    )
                })
        } else {
            panic!("Attempt to access invalid row.")
        }
    }

    pub fn drop_row_and_content(&mut self, row: RowIndex) {
        if self.is_row_initalized(row) {
            for column in self.components.values_mut() {
                // Safety: We have verified above that the row is valid and initalized.
                unsafe { column.drop_row(row) };
            }
        }

        self.drop_row(row);
    }
}

impl Drop for ComponentStorage {
    fn drop(&mut self) {
        // So we need to drop all the values we're storing.
        for column in self.components.values_mut() {
            for index in 0..self.next_row.0 {
                let index = RowIndex(index);
                // Make sure we don't drop and already freed row.
                if !self.free_rows.contains(&index) {
                    // Safety: This is safe because we checked that the value was still owned by us.
                    unsafe { column.drop_row(index) }
                }
            }
        }
    }
}
