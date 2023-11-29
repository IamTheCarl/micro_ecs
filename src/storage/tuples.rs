use unique_type_id::UniqueTypeId;

use crate::BusWidth;

use super::{
    Column, ComponentId, {any_as_bytes, any_from_bytes, ComponentStorage, RowIndex},
};
use fortuples::fortuples;

pub trait StorageAccessor<'a>: 'a {
    fn access(storage: &'a mut ComponentStorage, row: RowIndex) -> Self;
}

#[rustfmt::skip]
fortuples! {
    impl<'a> StorageAccessor<'a> for (#(&'a mut #Member),*)
    where
        #(#Member: UniqueTypeId<BusWidth> + 'static),*
    {
	#[allow(clippy::unused_unit)]
	fn access(storage: &'a mut ComponentStorage, row: RowIndex) -> (#(&'a mut #Member),*) {
            let components = [#(#Member::id()),*];
            let [#(casey::lower!(#Member)),*] = storage.access_row_raw(row, components);

            #(let casey::lower!(#Member) = casey::lower!(#Member) as *mut _ as *mut #Member;)*
	    
            (#(unsafe { &mut *casey::lower!(#Member) }),*)
        }
    }
}

pub trait ComponentSet {
    fn components() -> impl Iterator<Item = ComponentId>;
    fn columns() -> impl Iterator<Item = (ComponentId, Column)>;
    fn contains_component(component_id: ComponentId) -> bool;
}

#[rustfmt::skip]
fortuples! {
    impl ComponentSet for #Tuple
    where
        #(#Member: UniqueTypeId<BusWidth> + 'static),*
    {
	fn components() -> impl Iterator<Item = ComponentId> {
	    [#(#Member::id()),*].into_iter()
	}

	fn columns() -> impl Iterator<Item = (ComponentId, Column)> {
	    [#((#Member::id(), Column::new::<#Member>())),*].into_iter()
	}

	fn contains_component(component_id: ComponentId) -> bool {
	    [#(#Member::id()),*].contains(&component_id)
	}
    }
}

pub trait ReduceArgument {
    /// # Safety
    /// Components removed from the storage must not be dropped.
    unsafe fn build(storage: &mut ComponentStorage, row: RowIndex) -> Self;
}

#[rustfmt::skip]
fortuples! {
    impl ReduceArgument for #Tuple
    where
        #(#Member: UniqueTypeId<BusWidth> + 'static),*
    {	
	#[allow(clippy::unused_unit)]
	unsafe fn build(storage: &mut ComponentStorage, row: RowIndex) -> Self {
            let components = [#(#Member::id()),*];
            let [#(casey::lower!(#Member)),*] = storage.access_row_raw(row, components);
	    
            #(let casey::lower!(#Member) = unsafe { any_from_bytes::<#Member>(casey::lower!(#Member)) };)*
	    
	    (#(casey::lower!(#Member)),*)
	}
    }
}

pub trait StorageInsert {
    fn get_values(&self) -> impl Iterator<Item = (ComponentId, &[u8])>;
}

#[rustfmt::skip]
fortuples! {
    impl StorageInsert for #Tuple
    where
        #(#Member: UniqueTypeId<BusWidth> + 'static),*
    {
	fn get_values(&self) -> impl Iterator<Item = (ComponentId, &[u8])> {
            [#((#Member::id(), any_as_bytes(&#self))),*].into_iter()
	}
    }
}
