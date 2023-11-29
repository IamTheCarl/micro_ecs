#![no_std]
#![feature(allocator_api)]
#![feature(get_many_mut)]

extern crate alloc;

use core::num::NonZeroU16;

use alloc::vec::Vec;
use hashbrown::HashMap;

type BusWidth = u16;
type NonZeroBusWidth = NonZeroU16;

mod storage;
use storage::{ComponentSet, ComponentStorage, ReduceArgument, RowIndex, StorageAccessor};
use unique_type_id::{TypeId, UniqueTypeId};

use self::storage::{Column, StorageInsert};

#[derive(Debug, Hash, Eq, PartialEq, Clone, Copy)]
pub struct EntityId(NonZeroBusWidth);

#[derive(Debug, Hash, Eq, PartialEq, Clone, Copy)]
struct ArcheTypeId(BusWidth);

type ComponentId = TypeId<BusWidth>;

const EMPTY_ARCHE_TYPE_ID: ArcheTypeId = ArcheTypeId(0);

struct ArcheType {
    component_storage: ComponentStorage,
    add_component: HashMap<ComponentId, ArcheTypeId>,
    remove_component: HashMap<ComponentId, ArcheTypeId>,
}

impl ArcheType {
    /// Create a new arche type that can store the components of this one but with storage for component T added.
    pub fn extend(&self, component_id: ComponentId, column: Column) -> Self {
        Self {
            component_storage: self.component_storage.extend(component_id, column),
            add_component: HashMap::new(),
            remove_component: HashMap::new(),
        }
    }

    /// Create a new arche type that can store the components of this one but with storage for component T removed.
    pub fn reduce(&self, component_id: ComponentId) -> Self {
        Self {
            component_storage: self.component_storage.reduce(component_id),
            add_component: HashMap::new(),
            remove_component: HashMap::new(),
        }
    }
}

pub struct EntityAccess<'a> {
    record: &'a mut EntityRecord,
    arche_types: &'a mut Vec<ArcheType>,
    arche_type_lookup: &'a mut HashMap<Vec<ComponentId>, ArcheTypeId>,
}

impl<'a> EntityAccess<'a> {
    fn arche_type(&self) -> &ArcheType {
        self.arche_types
            .get(self.record.arche_type_id.0 as usize)
            .expect("Arche type did not exist.")
    }

    fn arche_type_mut(&mut self) -> &mut ArcheType {
        self.arche_types
            .get_mut(self.record.arche_type_id.0 as usize)
            .expect("Arche type did not exist.")
    }

    fn find_arche_type(&self, components: &[ComponentId]) -> Option<ArcheTypeId> {
        self.arche_type_lookup.get(components).cloned()
    }

    fn arche_type_change<T>(
        &mut self,
        graph_search: impl Fn(&ArcheType, ComponentId) -> Option<ArcheTypeId>,
        graph_update: impl Fn(&mut ArcheType, ComponentId, ArcheTypeId),
        build_component_set: impl Fn(&ArcheType, ComponentId) -> Vec<ComponentId>,
        arche_mod: impl Fn(&ArcheType, ComponentId, Column) -> ArcheType,
    ) -> ArcheTypeId
    where
        T: ComponentSet,
    {
        let mut previous_arche_type_id = self.record.arche_type_id;

        for (component_id, column) in T::columns() {
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
                    let new_arche_type = arche_mod(current_arche_type, component_id, column);

                    let new_arche_type_id = ArcheTypeId(self.arche_types.len() as BusWidth);
                    self.arche_types.push(new_arche_type);
                    self.arche_type_lookup
                        .insert(components_key, new_arche_type_id);

                    new_arche_type_id
                };

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
                    .component_storage
                    .iter_component_ids()
                    .chain(core::iter::once(new_component_id))
                    .collect()
            },
            |old_arche_type, component_id, column| old_arche_type.extend(component_id, column),
        );

        let old_arche_type_id = self.record.arche_type_id;

        if old_arche_type_id != new_arche_type_id {
            self.record.arche_type_id = new_arche_type_id;

            let [old_arche_type, new_arche_type] = self
                .arche_types
                .get_many_mut([old_arche_type_id.0 as usize, new_arche_type_id.0 as usize])
                .expect("Could not get old and new arche types.");

            let old_row = self.record.row;
            self.record.row = old_arche_type.component_storage.extend_row_to_other(
                &mut new_arche_type.component_storage,
                old_row,
                components,
            );
        }
    }

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
                    .component_storage
                    .iter_component_ids()
                    .filter(|iter_id| *iter_id != next_component_id)
                    .collect()
            },
            |old_arche_type, component_id, _column| old_arche_type.reduce(component_id),
        );

        let old_arche_type_id = self.record.arche_type_id;
        self.record.arche_type_id = new_arche_type_id;

        let [old_arche_type, new_arche_type] = self
            .arche_types
            .get_many_mut([old_arche_type_id.0 as usize, new_arche_type_id.0 as usize])
            .expect("Could not get old and new arche types.");

        let old_row = self.record.row;
        let (new_row, removed_components) = old_arche_type
            .component_storage
            .reduce_row_to_other::<T>(&mut new_arche_type.component_storage, old_row);

        self.record.row = new_row;
        removed_components
    }

    pub fn has_component<T>(&self) -> bool
    where
        T: UniqueTypeId<BusWidth>,
    {
        let arche_type = self.arche_type();

        arche_type.component_storage.contains_component::<T>()
    }

    pub fn access_components_mut<'b, A>(&'b mut self) -> A
    where
        A: StorageAccessor<'b>,
    {
        let row = self.record.row;

        self.arche_type_mut().component_storage.access_row(row)
    }
}

struct EntityRecord {
    arche_type_id: ArcheTypeId,
    row: RowIndex,
}

pub struct World {
    entities: HashMap<EntityId, EntityRecord>,
    next_entity_id: u16,
    arche_types: Vec<ArcheType>,
    arche_type_lookup: HashMap<Vec<ComponentId>, ArcheTypeId>,
}

impl Default for World {
    fn default() -> Self {
        let empty_arche_type = ArcheType {
            component_storage: ComponentStorage::empty_type(),
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
    fn next_entity_id(&mut self) -> EntityId {
        let entity_id = self.next_entity_id;
        self.next_entity_id += 1;

        EntityId(NonZeroBusWidth::new(entity_id).expect("Next entity ID was zero."))
    }

    pub fn create_empty_entity(&mut self) -> EntityId {
        let entity_id = self.next_entity_id();
        let arche_type = self
            .arche_types
            .get_mut(EMPTY_ARCHE_TYPE_ID.0 as usize)
            .unwrap();

        let row = arche_type.component_storage.insert_row(());

        self.entities.insert(
            entity_id,
            EntityRecord {
                arche_type_id: EMPTY_ARCHE_TYPE_ID,
                row,
            },
        );

        entity_id
    }

    pub fn entity_exists(&self, entity_id: EntityId) -> bool {
        self.entities.contains_key(&entity_id)
    }

    pub fn access_entity(&mut self, entity_id: EntityId) -> Option<EntityAccess> {
        let record = self.entities.get_mut(&entity_id)?;

        let access = EntityAccess {
            record,
            arche_types: &mut self.arche_types,
            arche_type_lookup: &mut self.arche_type_lookup,
        };

        Some(access)
    }

    pub fn drop_entity(&mut self, entity_id: EntityId) -> bool {
        let record = self.entities.remove(&entity_id);

        if let Some(record) = record {
            // We need to drop its storage.
            self.arche_types
                .get_mut(record.arche_type_id.0 as usize)
                .expect("Arche type for entity does not exist.")
                .component_storage
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
        #[UniqueTypeIdType = "BusWidth"]
        struct A(i32);

        #[derive(UniqueTypeId)]
        #[UniqueTypeIdFile = "testing_types.toml"]
        #[UniqueTypeIdType = "BusWidth"]
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

        let arche_type = world.arche_types.get(ARCHE_TYPE_A.0 as usize).unwrap();
        assert_eq!(
            arche_type.add_component,
            HashMap::from([(component_id_b, ARCHE_TYPE_AB),])
        );
        assert_eq!(
            arche_type.remove_component,
            HashMap::from([(component_id_a, EMPTY_ARCHE_TYPE_ID)])
        );

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
            core::mem::size_of::<BusWidth>()
        );
    }
}
