use alloc::{boxed::Box, vec::Vec};
use core::{marker::PhantomData, result::Result};
use fortuples::fortuples;
use hashbrown::{HashMap, HashSet};
use itertools::Itertools;
use thiserror::Error;
use unique_type_id::UniqueTypeId;

use crate::{bus, ArcheTypeId, ComponentId, World};

/// A schedule is what runs our systems on our entities/components.
/// You will provide it stages. A stage is a set of systems that can safely be ran in parallel.
/// Stages will be ran in the order they are supplied.
pub struct Schedule {
    stages: Vec<Stage>,
}

impl Schedule {
    pub fn new(stages: impl IntoIterator<Item = Stage>) -> Self {
        Self {
            stages: stages.into_iter().collect(),
        }
    }

    /// Iterate over each stage of the schedule.
    /// Why don't we just return an iterator? Well that's because it's
    /// unsafe to execute multiple stages in parallel, so we can't let you
    /// hold multiple stages in parallel.
    pub fn iter_stages(&self, mut closure: impl FnMut(&Stage)) {
        for stage in self.stages.iter() {
            closure(stage)
        }
    }
}

/// A stage is a set of systems that can safetly be ran in parallel.
/// In the case that you're using an executor that can't run in parallel, the order that they are ran will be
/// arbitrary.
#[derive(Debug, Eq, PartialEq, Default)]
pub struct Stage {
    systems: Vec<Box<dyn System>>,
}

impl Stage {
    pub fn from_systems(
        systems: impl IntoIterator<Item = Box<dyn System>>,
    ) -> Result<Self, StageConflictError> {
        let mut stage = Self::default();

        for system in systems.into_iter() {
            stage.add_boxed_system(system)?;
        }

        Ok(stage)
    }

    pub fn iter_systems(&self) -> impl Iterator<Item = &dyn System> {
        self.systems.iter().map(|system| system.as_ref())
    }

    pub fn add_boxed_system(
        &mut self,
        new_system: Box<dyn System>,
    ) -> Result<(), StageConflictError> {
        // Validate that the new system doesn't conflict with any of the already existing systems.
        for system in self.systems.iter() {
            for present_parameter in system.parameters() {
                for new_parameter in new_system.parameters() {
                    new_parameter
                        .check_for_parameter_conflict(present_parameter)
                        .map_err(|system_conflict| StageConflictError {
                            source: system_conflict,
                            system_a: system.name(),
                            system_b: new_system.name(),
                        })?;
                }
            }
        }

        // If we got here, then it means none of the old systems conflicted with the new one.
        self.systems.push(new_system);

        Ok(())
    }

    pub fn add_system<T>(&mut self, system: impl IntoSystem<T>) -> Result<(), AddSystemError> {
        // This already checks that the system doesn't have any internal conflicts.
        let new_system = system.into_system()?;

        self.add_boxed_system(new_system)?;

        Ok(())
    }
}

#[derive(Error, Debug)]
pub enum AddSystemError {
    #[error("Failed to convert into system {0}")]
    ConvertToSystem(#[from] SystemParameterConflictError),

    #[error("Conflict between systems {0}")]
    SystemConflict(#[from] StageConflictError),
}

#[derive(Error, Debug)]
#[error("{system_a} and {system_b}: {source}")]
pub struct StageConflictError {
    source: SystemParameterConflictError,
    system_a: &'static str,
    system_b: &'static str,
}

// impl Display for StageConflictError {}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum SystemParameterRequest {
    Query(HashSet<ComponentRequest>),
    Resource(ResourceRequest),
}

impl SystemParameterRequest {
    fn iter_arche_types<'a>(
        component_requests: &'a HashSet<ComponentRequest>,
        arche_type_lookup: &'a HashMap<Vec<ComponentId>, ArcheTypeId>,
    ) -> impl Iterator<Item = ArcheTypeId> + 'a {
        arche_type_lookup
            .iter()
            .filter(|(components, _arche_type_id)| {
                for request in component_requests.iter() {
                    match request {
                        ComponentRequest::Component(component_id)
                        | ComponentRequest::ComponentMut(component_id)
                        | ComponentRequest::With(component_id) => {
                            // Arche type must contain this component.
                            if !components.contains(component_id) {
                                return false;
                            }
                        }
                        ComponentRequest::Without(component_id) => {
                            // Arche type must not contain this component.
                            if components.contains(component_id) {
                                return false;
                            }
                        }
                    }
                }

                true
            })
            .map(|(_components, arche_type_id)| *arche_type_id)
    }

    fn check_for_component_overlap(
        our_requests: &HashSet<ComponentRequest>,
        their_requests: &HashSet<ComponentRequest>,
    ) -> bool {
        for our_request in our_requests.iter() {
            match our_request {
                ComponentRequest::With(component_id)
                | ComponentRequest::Component(component_id)
                | ComponentRequest::ComponentMut(component_id) => {
                    if their_requests.contains(&ComponentRequest::Without(*component_id)) {
                        // Other query demands they not have this component, which means we can't overlap.
                        return false;
                    }
                }
                ComponentRequest::Without(component_id) => {
                    if their_requests.contains(&ComponentRequest::With(*component_id))
                        || their_requests.contains(&ComponentRequest::Component(*component_id))
                        || their_requests.contains(&ComponentRequest::ComponentMut(*component_id))
                    {
                        // Other query demands they have this component, which means we can't overlap.
                        return false;
                    }
                }
            }
        }

        true
    }

    fn check_for_component_conflict(
        our_requests: &HashSet<ComponentRequest>,
        their_requests: &HashSet<ComponentRequest>,
    ) -> Result<(), ComponentConflictError> {
        // Do we have a `Without` or `With` filter to make sure we can't overlap?
        if Self::check_for_component_overlap(our_requests, their_requests) {
            // They do. We need to check for mutable aliacing or mutable and immutable borrows.
            for our_request in our_requests.iter() {
                match our_request {
                    ComponentRequest::Component(component_id) => {
                        if their_requests.contains(&ComponentRequest::ComponentMut(*component_id)) {
                            // Mutable reference while we have an immutable reference.
                            return Err(ComponentConflictError::MutableAndImmutableBorrow(
                                *component_id,
                            ));
                        }
                    }
                    ComponentRequest::ComponentMut(component_id) => {
                        if their_requests.contains(&ComponentRequest::ComponentMut(*component_id)) {
                            // We both have a mutable reference.
                            return Err(ComponentConflictError::MutableAliacing(*component_id));
                        }

                        if their_requests.contains(&ComponentRequest::Component(*component_id)) {
                            // We have a mutable reference while they have an immutable reference.
                            return Err(ComponentConflictError::MutableAndImmutableBorrow(
                                *component_id,
                            ));
                        }
                    }
                    _ => {} // We tested the with and without situations already, so there's no need to account for them anymore.
                }
            }
        }

        // No problems were found.
        Ok(())
    }

    fn check_for_resource_conflict(
        our_request: &ResourceRequest,
        their_request: &ResourceRequest,
    ) -> Result<(), ResourceConflictError> {
        match (our_request, their_request) {
            (
                ResourceRequest::Resource(_our_resource_id),
                ResourceRequest::Resource(_their_resource_id),
            ) => Ok(()), // Both are immutable. Even if they're accessing the same component, that's not an error.
            (
                ResourceRequest::Resource(our_resource_id),
                ResourceRequest::ResourceMut(their_resource_id),
            )
            | (
                ResourceRequest::ResourceMut(our_resource_id),
                ResourceRequest::Resource(their_resource_id),
            ) => {
                if our_resource_id == their_resource_id {
                    Err(ResourceConflictError::MutableAndImmutableBorrow(
                        *our_resource_id,
                    ))
                } else {
                    Ok(())
                }
            }
            (
                ResourceRequest::ResourceMut(our_resource_id),
                ResourceRequest::ResourceMut(their_resource_id),
            ) => {
                if our_resource_id == their_resource_id {
                    Err(ResourceConflictError::MutableAliacing(*our_resource_id))
                } else {
                    Ok(())
                }
            }
        }
    }

    fn check_for_parameter_conflict(
        &self,
        other: &Self,
    ) -> Result<(), SystemParameterConflictError> {
        match (self, other) {
            (Self::Query(our_requests), Self::Query(their_requests)) => {
                Self::check_for_component_conflict(our_requests, their_requests)?;
                Ok(())
            }
            (Self::Resource(our_request), Self::Resource(their_request)) => {
                Self::check_for_resource_conflict(our_request, their_request)?;
                Ok(())
            }
            _ => Ok(()), // If they are not the same kind of request, then they can't conflict.
        }
    }
}

#[derive(Debug, Error)]
pub enum ComponentConflictError {
    #[error("Attempted to have multiple mutable references to type {0:?}. Try using With and Without filters to fix this situation.")]
    MutableAliacing(ComponentId),

    #[error("There are both mutable and immutable borrows of type {0:?}. Try using With and Without filters to fix this situation.")]
    MutableAndImmutableBorrow(ComponentId),
}

#[derive(Debug, Error)]
pub enum ResourceConflictError {
    #[error("Attempted to have multiple mutable references to type {0:?}. You must put these systems into seprate stages.")]
    MutableAliacing(ComponentId),

    #[error("There are both mutable and immutable borrows of type {0:?}. You must put these systems into seprate stages.")]
    MutableAndImmutableBorrow(ComponentId),
}

#[derive(Debug, Error)]
pub enum SystemParameterConflictError {
    #[error("Conflict between requested components of queries: {0}")]
    Component(#[from] ComponentConflictError),

    #[error("Conflict between requested resource: {0}")]
    Resource(#[from] ResourceConflictError),
}

trait IntoSystemParameterRequest {
    fn into_system_parameter_request() -> SystemParameterRequest;
}

pub trait System {
    fn name(&self) -> &'static str;
    fn parameters(&self) -> &[SystemParameterRequest];
    fn run(&self, world: &World);
}

impl Eq for dyn System {}

impl PartialEq for dyn System {
    fn eq(&self, other: &Self) -> bool {
        // We have the same name and the same parameters. Gotta be the same.
        self.name() == other.name() && self.parameters() == other.parameters()
    }
}

impl core::fmt::Debug for dyn System {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut tuple = f.debug_tuple(self.name());

        for parameter in self.parameters() {
            tuple.field(parameter);
        }

        tuple.finish()
    }
}

pub trait IntoSystem<T> {
    fn into_system(self) -> Result<Box<dyn System>, SystemParameterConflictError>;
}

#[rustfmt::skip]
fortuples! {
    impl<F> IntoSystem<#Tuple> for F
    where
	#(#Member: IntoSystemParameterRequest + 'static),*
	F: Fn(#(#Member),*) + core::any::Any,
    {
        fn into_system(self) -> Result<Box<dyn System>, SystemParameterConflictError> {

	    struct CustomSystem {
		name: &'static str,
		parameters: [SystemParameterRequest; #len(Tuple)],
	    }

	    impl System for CustomSystem {
		fn name(&self) -> &'static str {
		    self.name
		}
		
		fn parameters(&self) -> &[SystemParameterRequest] {
		    &self.parameters
		}

		
		fn run(&self, world: &World) {
		    // TODO we need to get our resources from the world and then pass them to our function.
		    todo!()
		}
	    }

	    let parameters: [SystemParameterRequest; #len(Tuple)] = [#(#Member::into_system_parameter_request()),*];
	    
	    for (a, b) in parameters.iter().tuple_combinations() {
		a.check_for_parameter_conflict(b)?;
	    }
	    
	    Ok(Box::new(CustomSystem {
		name: core::any::type_name::<F>(),
	    	parameters
	    })) 
        }
    }
}

#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub enum ResourceRequest {
    Resource(ComponentId),
    ResourceMut(ComponentId),
}

#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub enum ComponentRequest {
    Component(ComponentId),
    ComponentMut(ComponentId),
    With(ComponentId),
    Without(ComponentId),
}

trait IntoComponentRequest {
    fn into_component_request() -> impl Iterator<Item = ComponentRequest>;
}

impl<T> IntoSystemParameterRequest for &T
where
    T: UniqueTypeId<bus::Width>,
{
    fn into_system_parameter_request() -> SystemParameterRequest {
        SystemParameterRequest::Resource(ResourceRequest::Resource(T::id()))
    }
}

impl<T> IntoSystemParameterRequest for &mut T
where
    T: UniqueTypeId<bus::Width>,
{
    fn into_system_parameter_request() -> SystemParameterRequest {
        SystemParameterRequest::Resource(ResourceRequest::ResourceMut(T::id()))
    }
}

pub struct Query<Q, F>
where
    Q: QueryArguments,
    F: QueryFilters,
{
    _query_arguments: PhantomData<Q>,
    _query_filters: PhantomData<F>,
}

impl<Q, F> IntoSystemParameterRequest for Query<Q, F>
where
    Q: QueryArguments,
    F: QueryFilters,
{
    fn into_system_parameter_request() -> SystemParameterRequest {
        let component_requests = Q::iter().chain(F::iter()).collect();

        SystemParameterRequest::Query(component_requests)
    }
}

pub trait QueryArgument {
    fn into_component_request() -> ComponentRequest;
}

impl<T> QueryArgument for &T
where
    T: UniqueTypeId<bus::Width>,
{
    fn into_component_request() -> ComponentRequest {
        ComponentRequest::Component(T::id())
    }
}
impl<T> QueryArgument for &mut T
where
    T: UniqueTypeId<bus::Width>,
{
    fn into_component_request() -> ComponentRequest {
        ComponentRequest::ComponentMut(T::id())
    }
}

pub trait QueryArguments {
    fn iter() -> impl Iterator<Item = ComponentRequest>;
}

#[rustfmt::skip]
fortuples! {
    impl QueryArguments for #Tuple
	where
	#(#Member: QueryArgument),*
    {
	fn iter() -> impl Iterator<Item = ComponentRequest> {
	    [#(#Member::into_component_request()),*].into_iter()
	}
    }
}

pub trait QueryFilter {
    fn into_component_request() -> ComponentRequest;
}

/// Only accepts entities without a paticular component.
pub struct Without<T>
where
    T: UniqueTypeId<bus::Width>,
{
    _type: PhantomData<T>,
}

impl<T> QueryFilter for Without<T>
where
    T: UniqueTypeId<bus::Width>,
{
    fn into_component_request() -> ComponentRequest {
        ComponentRequest::Without(T::id())
    }
}

/// Only accepts components with a paticular component.
pub struct With<T>
where
    T: UniqueTypeId<bus::Width>,
{
    _type: PhantomData<T>,
}

impl<T> QueryFilter for With<T>
where
    T: UniqueTypeId<bus::Width>,
{
    fn into_component_request() -> ComponentRequest {
        ComponentRequest::With(T::id())
    }
}

pub trait QueryFilters {
    fn iter() -> impl Iterator<Item = ComponentRequest>;
}

#[rustfmt::skip]
fortuples! {
    impl QueryFilters for #Tuple
	where
	#(#Member: QueryFilter),*
    {
	fn iter() -> impl Iterator<Item = ComponentRequest> {
	    [#(#Member::into_component_request()),*].into_iter()
	}
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::test::*;

    #[test]
    fn system_query_validation() {
        fn empty_system() {}
        assert!(empty_system.into_system().is_ok());

        fn simple_system(_: Query<(&A, &mut B), (Without<C>,)>) {}
        assert!(simple_system.into_system().is_ok());

        fn two_non_conflicting_queries(
            _: Query<(&A, &mut B), (Without<C>,)>,
            _: Query<(&C,), (Without<A>, Without<B>)>,
        ) {
        }
        assert!(two_non_conflicting_queries.into_system().is_ok());

        fn two_conflicting_queries(_: Query<(&A, &mut C), ()>, _: Query<(&C,), ()>) {}
        assert!(two_conflicting_queries.into_system().is_err());

        fn two_conflict_avoiding_queries(
            _: Query<(&A, &mut C), ()>,
            _: Query<(&C,), (Without<A>,)>,
        ) {
        }
        assert!(two_conflict_avoiding_queries.into_system().is_ok());

        fn two_conflict_avoiding_queries_using_only_filters(
            _: Query<(&mut C,), (With<A>,)>,
            _: Query<(&C,), (Without<A>,)>,
        ) {
        }
        assert!(two_conflict_avoiding_queries_using_only_filters
            .into_system()
            .is_ok());

        fn multiple_immutable_references(_: Query<(&C, &B), ()>, _: Query<(&C, &A), ()>) {}
        assert!(multiple_immutable_references.into_system().is_ok());

        fn multiple_mutable_references(_: Query<(&mut C, &B), ()>, _: Query<(&mut C, &A), ()>) {}
        assert!(multiple_mutable_references.into_system().is_err());
    }

    #[test]
    fn system_resource_validation() {
        fn empty_system() {}
        assert!(empty_system.into_system().is_ok());

        fn non_conflicting_system(_: &A, _: &B) {}
        assert!(non_conflicting_system.into_system().is_ok());

        fn non_conflicting_system_mut(_: &mut A, _: &mut B) {}
        assert!(non_conflicting_system_mut.into_system().is_ok());

        fn multiple_immutable_references(_: &A, _: &A) {}
        assert!(multiple_immutable_references.into_system().is_ok());

        fn mutable_aliace(_: &mut A, _: &mut A) {}
        assert!(mutable_aliace.into_system().is_err());

        fn immutable_and_mutable(_: &mut A, _: &A) {}
        assert!(immutable_and_mutable.into_system().is_err());
    }

    #[test]
    fn build_stage() {
        fn mutable_resource(_: &mut A) {}

        let mut stage = Stage::default();
        assert!(stage.add_system(mutable_resource).is_ok());
        assert!(stage.add_system(mutable_resource).is_err());

        assert!(Stage::from_systems([]).is_ok());
        assert!(Stage::from_systems([mutable_resource.into_system().unwrap()]).is_ok());
        assert!(Stage::from_systems([
            mutable_resource.into_system().unwrap(),
            mutable_resource.into_system().unwrap()
        ])
        .is_err());
    }

    #[test]
    fn build_schedule() {
        let schedule = Schedule::new([]);
        assert_eq!(schedule.stages.len(), 0);

        fn system_a() {}
        fn system_b() {}

        let schedule = Schedule::new([Stage::from_systems([
            system_a.into_system().unwrap(),
            system_b.into_system().unwrap(),
        ])
        .unwrap()]);

        assert_eq!(schedule.stages.len(), 1);
    }
}
