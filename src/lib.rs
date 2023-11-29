#![no_std]
#![feature(allocator_api)]
#![feature(get_many_mut)]

extern crate alloc;

use alloc::vec::Vec;
use hashbrown::HashMap;

#[cfg(any(
    all(
        feature = "bus_width_ptr",
        any(
            feature = "bus_width_u8",
            feature = "bus_width_u16",
            feature = "bus_width_u32",
            feature = "bus_width_u64"
        )
    ),
    all(
        feature = "bus_width_u8",
        any(
            feature = "bus_width_ptr",
            feature = "bus_width_u16",
            feature = "bus_width_u32",
            feature = "bus_width_u64"
        )
    ),
    all(
        feature = "bus_width_u16",
        any(
            feature = "bus_width_u8",
            feature = "bus_width_ptr",
            feature = "bus_width_u32",
            feature = "bus_width_u64"
        )
    ),
    all(
        feature = "bus_width_u32",
        any(
            feature = "bus_width_u8",
            feature = "bus_width_u16",
            feature = "bus_width_ptr",
            feature = "bus_width_u64"
        )
    ),
    all(
        feature = "bus_width_u64",
        any(
            feature = "bus_width_u8",
            feature = "bus_width_u16",
            feature = "bus_width_u32",
            feature = "bus_width_ptr"
        )
    ),
))]
compile_error!("You may only enable one bus width at a time.");

/// Many different devices have a different ideal bus width, and it often doesn't match the width of the processor's
/// pointer register. Because of that, usize is often not ideal.
/// By using cargo features, you can select `bus_width_ptr` (the default) to use usize as the integer type, but you can
/// also pick `bus_width_u8`, `bus_width_u16`, `bus_width_u32`, and `bus_width_u64`. Pick whatever is ideal for the memory
/// configuration of your device.
#[cfg(feature = "bus_width_ptr")]
pub mod bus {
    pub type Width = usize;
    pub type NonZero = core::num::NonZeroUsize;
}

#[cfg(feature = "bus_width_u8")]
pub mod bus {
    pub type Width = u8;
    pub type NonZero = core::num::NonZeroU8;
}

#[cfg(feature = "bus_width_u16")]
pub mod bus {
    pub type Width = u16;
    pub type NonZero = core::num::NonZeroU16;
}

#[cfg(feature = "bus_width_u32")]
pub mod bus {
    pub type Width = u32;
    pub type NonZero = core::num::NonZeroU32;
}

#[cfg(feature = "bus_width_u64")]
pub mod bus {
    pub type Width = u64;
    pub type NonZero = core::num::NonZeroU64;
}

mod storage;
use storage::{
    ColumnConstruction, ComponentSet, ComponentTable, ReduceArgument, RowIndex, StorageAccessor,
};
use unique_type_id::{TypeId, UniqueTypeId};

use self::storage::StorageInsert;

/// A reference to an entity stored in our world.
#[derive(Debug, Hash, Eq, PartialEq, Clone, Copy)]
pub struct EntityId(bus::NonZero);

/// Used to identify an arche type for an entity.
#[derive(Debug, Hash, Eq, PartialEq, Clone, Copy)]
struct ArcheTypeId(bus::Width);

/// A unique ID for a component, used to find components in tables.
type ComponentId = TypeId<bus::Width>;

/// The empty arche type, for types that don't contain any components.
const EMPTY_ARCHE_TYPE_ID: ArcheTypeId = ArcheTypeId(0);

/// Stores components for entities with a specific set of components.
/// Also stores information needed for the arche type graph, a graph for
/// quickly looking up other arche types when adding or removing components from an entity.
struct ArcheType {
    component_table: ComponentTable,
    add_component: HashMap<ComponentId, ArcheTypeId>,
    remove_component: HashMap<ComponentId, ArcheTypeId>,
}

impl ArcheType {
    /// Create a new arche type that can store the components of this one but with additional storage for the listed additional component.
    pub fn extend(&self, column_construction: ColumnConstruction) -> Self {
        Self {
            component_table: self.component_table.extend(column_construction),
            add_component: HashMap::new(),
            remove_component: HashMap::new(),
        }
    }

    /// Create a new arche type that can store the components of this one but with the storage for the listed component removed.
    pub fn reduce(&self, component_id: ComponentId) -> Self {
        Self {
            component_table: self.component_table.reduce(component_id),
            add_component: HashMap::new(),
            remove_component: HashMap::new(),
        }
    }
}

/// Used to efficiently access a single entity.
pub struct EntityAccess<'a> {
    record: &'a mut EntityRecord,
    arche_types: &'a mut Vec<ArcheType>,
    arche_type_lookup: &'a mut HashMap<Vec<ComponentId>, ArcheTypeId>,
}

impl<'a> EntityAccess<'a> {
    /// Get the arche type for this entity.
    fn arche_type(&self) -> &ArcheType {
        // Clippy doesn't like casting a usize to a usize, but we have to have the cast for when we're using u16 or some other type as the bus width.
        #[allow(clippy::unnecessary_cast)]
        self.arche_types
            .get(self.record.arche_type_id.0 as usize)
            .expect("Arche type did not exist.")
    }

    /// Get the arche type for this entity, as a mutable reference.
    fn arche_type_mut(&mut self) -> &mut ArcheType {
        // Clippy doesn't like casting a usize to a usize, but we have to have the cast for when we're using u16 or some other type as the bus width.
        #[allow(clippy::unnecessary_cast)]
        self.arche_types
            .get_mut(self.record.arche_type_id.0 as usize)
            .expect("Arche type did not exist.")
    }

    /// Find an arche type for a list of components. Returns none if an arche type with storage for this
    /// component set has not been created.
    fn find_arche_type(&self, components: &[ComponentId]) -> Option<ArcheTypeId> {
        self.arche_type_lookup.get(components).cloned()
    }

    /// Common code for arche type changes. The type of change done is determined by the provided closures.
    /// * `graph_search` - See if there is a known neghbor in the arche type graph that has the interested component added or removed.
    /// * `graph_update` - Update the arche type graph to be aware of a new component we just created.
    /// * `build_component_set` - Create a vec of components from the old arche type, plus or minus the additional listed component.
    /// * `arche_mod` - We need to create a modified version of the arche type that has had a column added or removed for an arche type.
    ///                 ColumnConstruction is used to safely construct the column.
    fn arche_type_change<T>(
        &mut self,
        graph_search: impl Fn(&ArcheType, ComponentId) -> Option<ArcheTypeId>,
        graph_update: impl Fn(&mut ArcheType, ComponentId, ArcheTypeId),
        build_component_set: impl Fn(&ArcheType, ComponentId) -> Vec<ComponentId>,
        arche_mod: impl Fn(&ArcheType, ColumnConstruction) -> ArcheType,
    ) -> ArcheTypeId
    where
        T: ComponentSet,
    {
        let mut previous_arche_type_id = self.record.arche_type_id;

        for column_construction in T::columns() {
            let component_id = column_construction.component_id();

            // Clippy doesn't like casting a usize to a usize, but we have to have the cast for when we're using u16 or some other type as the bus width.
            #[allow(clippy::unnecessary_cast)]
            let current_arche_type = self
                .arche_types
                .get(previous_arche_type_id.0 as usize)
                .unwrap();

            // Start by checking if it's in the arche type graph.
            let new_arche_type_id = graph_search(current_arche_type, component_id);

            let new_arche_type_id = if let Some(new_arche_type_id) = new_arche_type_id {
                // It was in the graph!
                new_arche_type_id
            } else {
                // It was not in the graph. We have to go look for it among all our arche types.
                let mut components_key = build_component_set(current_arche_type, component_id);
                components_key.sort();
                components_key.shrink_to_fit();

                let new_arche_type_id = self.find_arche_type(&components_key);

                let new_arche_type_id = if let Some(new_arche_type) = new_arche_type_id {
                    new_arche_type
                } else {
                    // Oh... it just doesn't exist. We'll have to create it.

                    // Columns are spawned empty, meaning they didn't allocate on the heap.
                    // Spawning an empty column that is then instantly dropped (like in the case of reducing) is practically a free operation.
                    let new_arche_type = arche_mod(current_arche_type, column_construction);

                    let new_arche_type_id = ArcheTypeId(self.arche_types.len() as bus::Width);
                    self.arche_types.push(new_arche_type);
                    self.arche_type_lookup
                        .insert(components_key, new_arche_type_id);

                    new_arche_type_id
                };

                // Clippy doesn't like casting a usize to a usize, but we have to have the cast for when we're using u16 or some other type as the bus width.
                #[allow(clippy::unnecessary_cast)]
                // Insert it into the graph so we can find it quickly in the future.
                let previous_arche_type = self
                    .arche_types
                    .get_mut(previous_arche_type_id.0 as usize)
                    .expect("Arche type did not exist.");
                graph_update(previous_arche_type, component_id, new_arche_type_id);

                new_arche_type_id
            };

            previous_arche_type_id = new_arche_type_id;
        }

        previous_arche_type_id
    }

    /// Insert one or more components into this entity.
    pub fn insert_components<T>(&mut self, components: T)
    where
        T: ComponentSet + StorageInsert,
    {
        let new_arche_type_id = self.arche_type_change::<T>(
            |old_arche_type, component_id| old_arche_type.add_component.get(&component_id).copied(),
            |old_arche_type, component_id, new_arche_type_id| {
                old_arche_type
                    .add_component
                    .insert(component_id, new_arche_type_id);
            },
            |old_arche_type, new_component_id| {
                old_arche_type
                    .component_table
                    .iter_component_ids()
                    .chain(core::iter::once(new_component_id))
                    .collect()
            },
            |old_arche_type, column_construction| old_arche_type.extend(column_construction),
        );

        let old_arche_type_id = self.record.arche_type_id;

        if old_arche_type_id != new_arche_type_id {
            self.record.arche_type_id = new_arche_type_id;

            // Clippy doesn't like casting a usize to a usize, but we have to have the cast for when we're using u16 or some other type as the bus width.
            #[allow(clippy::unnecessary_cast)]
            let [old_arche_type, new_arche_type] = self
                .arche_types
                .get_many_mut([old_arche_type_id.0 as usize, new_arche_type_id.0 as usize])
                .expect("Could not get old and new arche types.");

            let old_row = self.record.row;
            self.record.row = old_arche_type.component_table.extend_row_to_other(
                &mut new_arche_type.component_table,
                old_row,
                components,
            );
        }
    }

    /// Remove one or more components from this entity.
    pub fn remove_components<T>(&mut self) -> T
    where
        T: ReduceArgument + ComponentSet,
    {
        let new_arche_type_id = self.arche_type_change::<T>(
            |old_arche_type, component_id| {
                old_arche_type.remove_component.get(&component_id).copied()
            },
            |old_arche_type, component_id, next_arche_type_id| {
                old_arche_type
                    .remove_component
                    .insert(component_id, next_arche_type_id);
            },
            |old_arche_type, next_component_id| {
                old_arche_type
                    .component_table
                    .iter_component_ids()
                    .filter(|iter_id| *iter_id != next_component_id)
                    .collect()
            },
            |old_arche_type, column_construction| {
                old_arche_type.reduce(column_construction.component_id())
            },
        );

        let old_arche_type_id = self.record.arche_type_id;
        self.record.arche_type_id = new_arche_type_id;

        // Clippy doesn't like casting a usize to a usize, but we have to have the cast for when we're using u16 or some other type as the bus width.
        #[allow(clippy::unnecessary_cast)]
        let [old_arche_type, new_arche_type] = self
            .arche_types
            .get_many_mut([old_arche_type_id.0 as usize, new_arche_type_id.0 as usize])
            .expect("Could not get old and new arche types.");

        let old_row = self.record.row;
        let (new_row, removed_components) = old_arche_type
            .component_table
            .reduce_row_to_other::<T>(&mut new_arche_type.component_table, old_row);

        self.record.row = new_row;
        removed_components
    }

    /// Return true if the entity contains this component.
    pub fn has_component<T>(&self) -> bool
    where
        T: UniqueTypeId<bus::Width>,
    {
        let arche_type = self.arche_type();

        arche_type.component_table.contains_component::<T>()
    }

    /// Get a mutable reference to the components of this entity.
    pub fn access_components_mut<'b, A>(&'b mut self) -> A
    where
        A: StorageAccessor<'b>,
    {
        let row = self.record.row;

        self.arche_type_mut().component_table.access_row(row)
    }
}

/// A record of the arche type of a component and the row in the component table it can be found in.
struct EntityRecord {
    arche_type_id: ArcheTypeId,
    row: RowIndex,
}

/// An ECS world that stores all your entities.
pub struct World {
    entities: HashMap<EntityId, EntityRecord>,
    next_entity_id: bus::Width,
    arche_types: Vec<ArcheType>,
    arche_type_lookup: HashMap<Vec<ComponentId>, ArcheTypeId>,
}

impl Default for World {
    fn default() -> Self {
        let empty_arche_type = ArcheType {
            component_table: ComponentTable::empty_type(),
            add_component: HashMap::new(),
            remove_component: HashMap::new(),
        };

        Self {
            entities: HashMap::default(),
            next_entity_id: 1,
            arche_types: Vec::from([empty_arche_type]),
            arche_type_lookup: HashMap::from([(Vec::new(), EMPTY_ARCHE_TYPE_ID)]),
        }
    }
}

impl World {
    /// Get an entity ID. It needs to be unique and never been used before (just in case someone is holding a reference to the dead entity)
    fn next_entity_id(&mut self) -> EntityId {
        let entity_id = self.next_entity_id;
        self.next_entity_id += 1;

        EntityId(bus::NonZero::new(entity_id).expect("Next entity ID was zero."))
    }

    /// Create a new entity that doesn't have any components stored in it yet.
    pub fn create_empty_entity(&mut self) -> EntityId {
        let entity_id = self.next_entity_id();
        // Clippy doesn't like casting a usize to a usize, but we have to have the cast for when we're using u16 or some other type as the bus width.
        #[allow(clippy::unnecessary_cast)]
        let arche_type = self
            .arche_types
            .get_mut(EMPTY_ARCHE_TYPE_ID.0 as usize)
            .unwrap();

        let row = arche_type.component_table.insert_row(());

        self.entities.insert(
            entity_id,
            EntityRecord {
                arche_type_id: EMPTY_ARCHE_TYPE_ID,
                row,
            },
        );

        entity_id
    }

    /// Returns true if the entity exists.
    pub fn entity_exists(&self, entity_id: EntityId) -> bool {
        self.entities.contains_key(&entity_id)
    }

    /// Access an entity for manipulation. You'll be able to add, remove, and manipulate the components of the entity.
    pub fn access_entity(&mut self, entity_id: EntityId) -> Option<EntityAccess> {
        let record = self.entities.get_mut(&entity_id)?;

        let access = EntityAccess {
            record,
            arche_types: &mut self.arche_types,
            arche_type_lookup: &mut self.arche_type_lookup,
        };

        Some(access)
    }

    /// Free an entity's memory, including its components.
    pub fn drop_entity(&mut self, entity_id: EntityId) -> bool {
        let record = self.entities.remove(&entity_id);

        if let Some(record) = record {
            // Clippy doesn't like casting a usize to a usize, but we have to have the cast for when we're using u16 or some other type as the bus width.
            #[allow(clippy::unnecessary_cast)]
            // We need to drop its storage.
            self.arche_types
                .get_mut(record.arche_type_id.0 as usize)
                .expect("Arche type for entity does not exist.")
                .component_table
                .drop_row_and_content(record.row);

            // This entity existed, and was removed.
            true
        } else {
            // This entity did not exist, and therefore was not removed.
            false
        }
    }
}

#[cfg(test)]
mod test {
    use unique_type_id_derive::UniqueTypeId;

    use super::*;

    #[test]
    fn create_entity() {
        let mut world = World::default();

        let entity_id = world.create_empty_entity();
        assert!(world.entity_exists(entity_id));
    }

    #[test]
    fn insert_remove_components() {
        let mut world = World::default();

        #[derive(UniqueTypeId)]
        #[UniqueTypeIdFile = "testing_types.toml"]
        #[UniqueTypeIdType = "bus::Width"]
        struct A(i32);

        #[derive(UniqueTypeId)]
        #[UniqueTypeIdFile = "testing_types.toml"]
        #[UniqueTypeIdType = "bus::Width"]
        struct B(f32);

        let component_id_a: ComponentId = A::id();
        let component_id_b: ComponentId = B::id();

        const ARCHE_TYPE_A: ArcheTypeId = ArcheTypeId(1);
        const ARCHE_TYPE_B: ArcheTypeId = ArcheTypeId(3);
        const ARCHE_TYPE_AB: ArcheTypeId = ArcheTypeId(2);

        let entity_id = world.create_empty_entity();

        // Insert and remove a bunch of components. Make sure we keep our arche types right.
        let mut entity = world.access_entity(entity_id).unwrap();
        assert!(!entity.has_component::<A>());
        assert!(!entity.has_component::<B>());
        assert_eq!(entity.record.arche_type_id, EMPTY_ARCHE_TYPE_ID);

        entity.insert_components((A(42),));
        assert!(entity.has_component::<A>());
        assert!(!entity.has_component::<B>());
        assert_eq!(entity.record.arche_type_id, ARCHE_TYPE_A);

        let (old_a,) = entity.remove_components::<(A,)>();
        assert_eq!(old_a.0, 42);
        assert!(!entity.has_component::<A>());
        assert!(!entity.has_component::<B>());
        assert_eq!(entity.record.arche_type_id, EMPTY_ARCHE_TYPE_ID);

        entity.insert_components((A(32), B(3.00)));
        assert!(entity.has_component::<A>());
        assert!(entity.has_component::<B>());
        assert_eq!(entity.record.arche_type_id, ARCHE_TYPE_AB);

        let (old_a,) = entity.remove_components::<(A,)>();
        assert_eq!(old_a.0, 32);
        assert!(!entity.has_component::<A>());
        assert!(entity.has_component::<B>());
        assert_eq!(entity.record.arche_type_id, ARCHE_TYPE_B);

        let (old_b,) = entity.remove_components::<(B,)>();
        assert_eq!(old_b.0, 3.00);
        assert!(!entity.has_component::<A>());
        assert!(!entity.has_component::<B>());
        assert_eq!(entity.record.arche_type_id, EMPTY_ARCHE_TYPE_ID);

        entity.insert_components((B(5.00), A(24)));
        assert!(entity.has_component::<A>());
        assert!(entity.has_component::<B>());
        assert_eq!(entity.record.arche_type_id, ARCHE_TYPE_AB);

        let (a, b) = entity.access_components_mut::<(&mut A, &mut B)>();
        assert_eq!(a.0, 24);
        assert_eq!(b.0, 5.00);

        let (a, b) = entity.remove_components::<(A, B)>();
        assert_eq!(a.0, 24);
        assert_eq!(b.0, 5.00);

        let drop_result = world.drop_entity(entity_id);
        assert!(drop_result);

        // Clippy doesn't like casting a usize to a usize, but we have to have the cast for when we're using u16 or some other type as the bus width.
        #[allow(clippy::unnecessary_cast)]
        // Let's verify the arche type graph.
        let arche_type = world
            .arche_types
            .get(EMPTY_ARCHE_TYPE_ID.0 as usize)
            .unwrap();
        assert_eq!(
            arche_type.add_component,
            HashMap::from([
                (component_id_a, ARCHE_TYPE_A),
                (component_id_b, ARCHE_TYPE_B)
            ])
        );
        assert_eq!(arche_type.remove_component, HashMap::from([]));

        // Clippy doesn't like casting a usize to a usize, but we have to have the cast for when we're using u16 or some other type as the bus width.
        #[allow(clippy::unnecessary_cast)]
        let arche_type = world.arche_types.get(ARCHE_TYPE_A.0 as usize).unwrap();
        assert_eq!(
            arche_type.add_component,
            HashMap::from([(component_id_b, ARCHE_TYPE_AB),])
        );
        assert_eq!(
            arche_type.remove_component,
            HashMap::from([(component_id_a, EMPTY_ARCHE_TYPE_ID)])
        );

        // Clippy doesn't like casting a usize to a usize, but we have to have the cast for when we're using u16 or some other type as the bus width.
        #[allow(clippy::unnecessary_cast)]
        let arche_type = world.arche_types.get(ARCHE_TYPE_B.0 as usize).unwrap();
        assert_eq!(
            arche_type.add_component,
            HashMap::from([(component_id_a, ARCHE_TYPE_AB),])
        );
        assert_eq!(
            arche_type.remove_component,
            HashMap::from([(component_id_b, EMPTY_ARCHE_TYPE_ID)])
        );
    }

    #[test]
    fn component_id_size() {
        assert_eq!(
            core::mem::size_of::<ComponentId>(),
            core::mem::size_of::<bus::Width>()
        );
    }
}
