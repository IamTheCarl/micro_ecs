//! Components are passed as tuples. Rust doesn't formally support varadic args so to support this we
//! use insane numbers of generic tuples generated with marcros. This is a lot of code so we just put
//! it all here in this paticular module.

use unique_type_id::UniqueTypeId;

use crate::bus;

use super::{
    Column, ComponentId, {any_as_bytes, any_from_bytes, ComponentTable, RowIndex},
};
use fortuples::fortuples;

/// Used to access componets of an entity.
pub trait StorageAccessor<'a>: 'a {
    fn access(storage: &'a mut ComponentTable, row: RowIndex) -> Self;
}

#[rustfmt::skip]
fortuples! {
    impl<'a> StorageAccessor<'a> for (#(&'a mut #Member),*)
    where
        #(#Member: UniqueTypeId<bus::Width> + 'static),*
    {
	#[allow(clippy::unused_unit)]
	fn access(storage: &'a mut ComponentTable, row: RowIndex) -> (#(&'a mut #Member),*) {
            let components = [#(#Member::id()),*];
            let [#(casey::lower!(#Member)),*] = storage.access_row_raw(row, components);

            #(let casey::lower!(#Member) = casey::lower!(#Member) as *mut _ as *mut #Member;)*
	    
            (#(unsafe { &mut *casey::lower!(#Member) }),*)
        }
    }
}

/// It is unsafe to let the wrong ComponentId be associated with the wrong Column, so this
/// forces them to stay together.
pub struct ColumnConstruction(pub(super) ComponentId, pub(super) Column);
impl ColumnConstruction {
    pub(crate) fn component_id(&self) -> ComponentId {
        self.0
    }
}

/// Represents any tuple that qualifies as a set of components.
pub trait ComponentSet {
    fn components() -> impl Iterator<Item = ComponentId>;
    fn columns() -> impl Iterator<Item = ColumnConstruction>;
    fn contains_component(component_id: ComponentId) -> bool;
}

#[rustfmt::skip]
fortuples! {
    impl ComponentSet for #Tuple
    where
        #(#Member: UniqueTypeId<bus::Width> + 'static),*
    {
	fn components() -> impl Iterator<Item = ComponentId> {
	    [#(#Member::id()),*].into_iter()
	}

	fn columns() -> impl Iterator<Item = ColumnConstruction> {
	    [#(ColumnConstruction(#Member::id(), Column::new::<#Member>())),*].into_iter()
	}

	fn contains_component(component_id: ComponentId) -> bool {
	    [#(#Member::id()),*].contains(&component_id)
	}
    }
}

/// A set of components we intend to remove from storage.
pub trait ReduceArgument {
    /// # Safety
    /// Components removed from the storage must not be dropped by the table.
    /// It is safe for the components, once built, to be dropped.
    unsafe fn build(storage: &mut ComponentTable, row: RowIndex) -> Self;
}

#[rustfmt::skip]
fortuples! {
    impl ReduceArgument for #Tuple
    where
        #(#Member: UniqueTypeId<bus::Width> + 'static),*
    {	
	#[allow(clippy::unused_unit)]
	unsafe fn build(storage: &mut ComponentTable, row: RowIndex) -> Self {
            let components = [#(#Member::id()),*];
            let [#(casey::lower!(#Member)),*] = storage.access_row_raw(row, components);
	    
            #(let casey::lower!(#Member) = unsafe { any_from_bytes::<#Member>(casey::lower!(#Member)) };)*
	    
	    (#(casey::lower!(#Member)),*)
	}
    }
}

/// A set of components we intend to insert into storage.
pub trait StorageInsert {
    fn get_values(&self) -> impl Iterator<Item = (ComponentId, &[u8])>;
}

#[rustfmt::skip]
fortuples! {
    impl StorageInsert for #Tuple
    where
        #(#Member: UniqueTypeId<bus::Width> + 'static),*
    {
	fn get_values(&self) -> impl Iterator<Item = (ComponentId, &[u8])> {
            [#((#Member::id(), any_as_bytes(&#self))),*].into_iter()
	}
    }
}
