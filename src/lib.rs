//! This library provides an Event-Driven State Machine implementation for Rust
//!
//! A State Machine describes a struct that maintains a *State* variable in a predictable way,
//! using a set of *Transitions* to describe how the state may change. Transitions are triggered
//! by *Events*, which are generated by the environment, and may in turn cause *Effects* to execute,
//! which can influence the environment. State Machines may also carry arbitrary *Data*, which can
//! be mutated by Effects.
//!
//! In this State Machine implementation, the State Machine is operated by providing it with
//! Events through the [StateMachine::handle_event] method. Events can be anything, but it is common
//! to represent them with an Enum.
//!
//! States themselves are very restricted to allow for Any / AllOf matching, and typically should
//! also be implemented with an enum that derives Copy, Clone, Eq, PartialEq, and Debug.
//!
//! # Defining Transitions
//!
//! State Machines in statement are simply a thin wrapper over a state object and a list of
//! transitions. To define a state machine, you need to:
//! 1. Create a [StateMachineFactory] using [StateMachineFactory::new]
//! 2. Add transitions using one or more of:
//!     - [StateMachineFactory::with_predicated_transition]
//!     - [StateMachineFactory::with_predicated_transition_effect]
//!     - [StateMachineFactory::with_event_transition]
//!     - [StateMachineFactory::with_event_transition_effect]
//!     - [StateMachineFactory::with_auto_transition]
//!     - [StateMachineFactory::with_custom_transition]
//! 3. Lock your factory into a [LockedStateMachineFactory] by calling [StateMachineFactory::lock]
//! 4. Create a state machine by calling [LockedStateMachineFactory::build]
//!
//! # Transitions
//!
//! Transitions (represented by the [StateMachineTransition] struct) must specify the State or set
//! of initial states (as a [FromState]) that may trigger them:
//! - [FromState::Any]: Any starting state - this Transition will be evaluated for all events.
//! - [FromState::AnyOf]: Any starting state in the provided list.
//! - [FromState::From]: The specific provided started state. FromState implements [From] for this
//! variant, so the variant can be elided for the common case.
//!
//! Transitions may also optionally provide a predicate to apply custom logic to decide whether the
//! Transition is applied. Transitions may also be triggered from any ([FromState::Any]) state,
//! meaning that they are considered for any Event.
//!
//! Transitions must also describe the state that they transition the State Machine into. The to_state
//! of a transition can be represented as one of the following:
//! - [To]: A specific, pre-defined state. ToState implements [From] for this variant, so the variant
//! can be elided for the common case.
//! - [Same]: Whatever state the transition started from; this makes the transition a no-op for the
//! state machine, but side effects may still be executed. This is useful in some cases, such as in
//! transition loggers.
//! - [Calc]: Allows for dynamic target state calculation, when a given transition may result in
//! more than one target states. This is something of an antipattern; these should preferentially
//! be represented as multiple transitions with different predicates.
//!
//! # Event Lifecycle
//!
//! 1. Handle event called.
//! 2. For each defined transition:
//!
//!     2a. Determine if the from_state of the transition matches the current state.
//!         If false, break and move on to the next transition.
//!
//!     2b. Determine the to_state of the transition.
//!
//!     2c. Run the transition's predicate, if any.
//!         If false (or no predicate), break and move on to the next transition.
//!
//!     2d. Run the transition's effect, if any.
//!
//!     2e. Transition the state machine to the to_state determined in 2b above.
//!
//! 3. If the State Machine has cycle set to true, return to 2.
//!
#![deny(missing_docs)]

use std::fmt::{Debug};
use std::ops::Deref;
use std::sync::Arc;
use thiserror::Error;
use crate::ToState::{Calc, Same, To};

/// State Machine instance, usually created by calling create on a [LockedStateMachineFactory]
#[derive(Default, Clone)]
pub struct StateMachine<'a, TEvent, TState: PartialEq<TState> + Clone + Send + 'a, TData> {
    /// The current state of the `StateMachine`
    pub state: TState,
    /// All of the transitions that are valid for this state machine. Note that this list may be
    /// shared with other state machine instances.
    pub transitions: Arc<Vec<StateMachineTransition<'a, TEvent, TState, TData>>>,
    /// Data associated with this state machine instance. This may be used to track information that
    /// cannot be expressed conveniently in Events, or it may be data which Side Effects act on. In
    /// the latter case, `TData` may need to implement interior mutability.
    pub data: TData,
    /// True if this state machine automatically re-runs evaluation after a transition, potentially
    /// executing multiple state transitions for one event.
    pub cycle: bool,
}

impl <'a, TEvent, TState: PartialEq<TState> + Debug + Clone + Send + Eq + PartialEq + 'a, TData> StateMachine<'a, TEvent, TState, TData> {
    fn new(cycle: bool, initial_state: TState, initial_data: TData) -> Self {
        Self {
            cycle,
            state: initial_state,
            data: initial_data,
            transitions: Arc::new(Vec::new()),
        }
    }

    /// Creates a `StateMachine` from a pre-existing set of transitions.
    pub fn with_transitions(mut self, transitions: Arc<Vec<StateMachineTransition<'a, TEvent, TState, TData>>>) -> Self {
        self.transitions = transitions.clone();
        self
    }

    /// Handles an Event, causing the state machine to execute one or more Transitions.
    pub fn handle_event(&mut self, event: TEvent) -> Result<&TState, StateMachineError<TState>> {
        loop {
            let mut transition_occurred = false;
            for transition in self.transitions.deref() {

                // Determine if the current state matches the from_state of the transition
                let from_state_matches = match &transition.from_state {
                    FromState::Any => true,
                    FromState::AnyOf(states) => states.iter().any(|s|s == &self.state),
                    FromState::From(state) => state == &self.state
                };

                // If the from_state matches, we need to consider whether this transition should execute
                if from_state_matches {

                    // Determine the result state and whether we need to proceed after this transition
                    // If proceed is true OR this transition changes the state, we will continue to
                    // evaluate further transitions after executing this one.
                    let to_state = match &transition.get_to_state {
                        To(to_state) => to_state.clone(),
                        Calc(get_to_state) => {
                            let data = StateTransitionToStateData {
                                data: &mut self.data,
                                event: &event,
                                from: &self.state,
                            };
                            get_to_state.deref()(data)
                        },
                        Same => self.state.clone()
                    };

                    // This sets up a data item to pass to the Predicate method (if any) and the
                    // Effect method (if any)
                    let transition_effect_data = StateTransitionEffectData {
                        name: &transition.name,
                        data: &mut self.data,
                        event: &event,
                        from: &self.state,
                        to: &to_state
                    };

                    // If there is a Predicate on this Transition, execute it and if it returns
                    // false, skip to the next Transition
                    if let Some(predicate) = &transition.event_predicate {
                        if !predicate(&transition_effect_data) {
                            continue;
                        }
                    }

                    // If there is an Effect on this Transition, execute it
                    if let Some(effect) = &transition.effect {
                        effect(transition_effect_data)
                            .map_err(|e| StateMachineError::EffectError(self.state.clone(), to_state.clone(), e))?;
                    }

                    // If proceed is false or we changed state, mark transition_occurred as true so
                    // that we evaluate all of the transitions again.
                    if &self.state != &to_state {
                        self.state = to_state;
                        transition_occurred = true;
                    }
                }
            }

            // If no transition occurred, we can end evaluation
            if !self.cycle || !transition_occurred {
                break;
            }
        }
        Ok(&self.state)
    }
}

/// Locked Factory for StateMachines. This struct is created by calling .lock() on a
/// StateMachineFactory, usually after defining all transitions needed.
pub struct LockedStateMachineFactory<'a, TEvent, TState: PartialEq<TState> + Clone + Send + 'a, TData = ()> {
    transitions: Arc<Vec<StateMachineTransition<'a, TEvent, TState, TData>>>,
    cycle: bool,
}

impl <'a, TEvent, TState: PartialEq<TState> + Debug + Clone + Send + Eq + PartialEq + 'a, TData> LockedStateMachineFactory<'a, TEvent, TState, TData> {
    /// Builds a StateMachine with a specified initial state and initial data.
    pub fn build(&self, initial_state: TState, initial_data: TData) -> StateMachine<'a, TEvent, TState, TData> {
        StateMachine::new(self.cycle, initial_state, initial_data).with_transitions(self.transitions.clone())
    }
}

/// Factory for StateMachines. This struct can be used to define a series of Transitions that
/// may be subsequently used to create multiple state machine instances with those same
/// transitions.
#[derive(Default)]
pub struct StateMachineFactory<'a, TEvent, TState: PartialEq<TState> + Clone + Send + 'a, TData> {
    cycle: bool,
    transitions: Vec<StateMachineTransition<'a, TEvent, TState, TData>>,
}

impl <'a, TEvent, TState: PartialEq<TState> + Debug + Clone + Send + Eq + PartialEq + 'a, TData> StateMachineFactory<'a, TEvent, TState, TData> {
    /// Creates a new `StateMachineFactory`
    pub fn new() -> Self {
        Self {
            cycle: false,
            transitions: Vec::new(),
        }
    }

    /// Controls whether a state machine loops back after a transition.
    pub fn cycle(self, cycle: bool) -> Self {
        Self {
            cycle,
            transitions: self.transitions
        }
    }

    /// Creates a LockedStateMachineFactory which can be used to build StateMachine instances
    /// with the Transitions defined in this StateMachineFactory.
    pub fn lock(self) -> LockedStateMachineFactory<'a, TEvent, TState, TData> {
        LockedStateMachineFactory {
            cycle: self.cycle,
            transitions: Arc::new(self.transitions)
        }
    }

    /// Adds an externally-created transition to this `StateMachineFactory`
    pub fn with_custom_transition(mut self, transition: StateMachineTransition<'a, TEvent, TState, TData>) -> Self
    {
        self.transitions.push(transition);
        self
    }

    /// Adds a named Transition to the State Machine definition with no predicate and no side
    /// effects. If this State Machine has cycle enabled, this transition will execute
    /// automatically, essentially skipping the From state. If Cycle is not enabled, the State
    /// Machine will transition to the To state with any future event.
    pub fn with_named_auto_transition(mut self, name: impl Into<String>, from_state: impl Into<FromState<TState>>, get_to_state: impl Into<ToState<TEvent, TState, TData>>) -> Self
    {
        self.transitions.push(StateMachineTransition::new(Some(name.into()), None, from_state.into(), get_to_state.into(), None));
        self
    }

    /// Adds a named Transition to the State Machine definition with a side effect and no predicate.
    /// If this State Machine has cycle enabled, this transition will execute automatically,
    /// essentially skipping the From state after executing the side effect. If Cycle is not
    /// enabled, the State Machine will transition to the To state with any future event.
    pub fn with_named_transition_effect(mut self, name: impl Into<String>, from_state: impl Into<FromState<TState>>, get_to_state: impl Into<ToState<TEvent, TState, TData>>, effect: impl Fn(StateTransitionEffectData<TEvent, TState, TData>) -> Result<(), Box<dyn std::error::Error + Send>> + Send + 'a) -> Self
    {
        self.transitions.push(StateMachineTransition::new(Some(name.into()), None, from_state.into(), get_to_state.into(), Some(Box::new(effect))));
        self
    }

    /// Adds a name Transition to the State Machine definition with a predicate and no Side Effect.
    /// This transition will test the predicate for any event and move to the To state if the
    /// Predicate returns true.
    pub fn with_named_predicated_transition(mut self, name: impl Into<String>, from_state: impl Into<FromState<TState>>, get_to_state: impl Into<ToState<TEvent, TState, TData>>, event_predicate: impl Fn(&StateTransitionEffectData<TEvent, TState, TData>) -> bool + Send + 'a) -> Self
    {
        self.transitions.push(StateMachineTransition::new(Some(name.into()), Some(Box::new(event_predicate)), from_state.into(), get_to_state.into(), None));
        self
    }

    /// Adds a named Transition to the State Machine definition with a predicate and a Side Effect.
    /// This transition will test the predicate for any event and execute the Side Effect then move
    /// to the To state if the Predicate returns true.
    pub fn with_named_predicated_transition_effect(mut self, name: impl Into<String>, from_state: impl Into<FromState<TState>>, get_to_state: impl Into<ToState<TEvent, TState, TData>>, event_predicate: impl Fn(&StateTransitionEffectData<TEvent, TState, TData>) -> bool + Send + 'a, effect: impl Fn(StateTransitionEffectData<TEvent, TState, TData>) -> Result<(), Box<dyn std::error::Error + Send>> + Send + 'a) -> Self
    {
        self.transitions.push(StateMachineTransition::new(Some(name.into()), Some(Box::new(event_predicate)), from_state.into(), get_to_state.into(), Some(Box::new(effect))));
        self
    }

    /// Adds an unnamed Transition to the State Machine definition with no predicate and no side
    /// effects. If this State Machine has cycle enabled, this transition will execute
    /// automatically, essentially skipping the From state. If Cycle is not enabled, the State
    /// Machine will transition to the To state with any future event.
    pub fn with_auto_transition(mut self, from_state: impl Into<FromState<TState>>, get_to_state: impl Into<ToState<TEvent, TState, TData>>) -> Self
    {
        self.transitions.push(StateMachineTransition::new(None, None, from_state.into(), get_to_state.into(), None));
        self
    }

    /// Adds an unnamed Transition to the State Machine definition with a side effect and no
    /// predicate. If this State Machine has cycle enabled, this transition will execute
    /// automatically, essentially skipping the From state after executing the side effect. If
    /// Cycle is not enabled, the State Machine will transition to the To state with any future
    /// event.
    pub fn with_transition_effect(mut self, from_state: impl Into<FromState<TState>>, get_to_state: impl Into<ToState<TEvent, TState, TData>>, effect: impl Fn(StateTransitionEffectData<TEvent, TState, TData>) -> Result<(), Box<dyn std::error::Error + Send>> + Send + 'a) -> Self
    {
        self.transitions.push(StateMachineTransition::new(None, None, from_state.into(), get_to_state.into(), Some(Box::new(effect))));
        self
    }

    /// Adds an unnamed Transition to the State Machine definition with a predicate and no Side
    /// Effect. This transition will test the predicate for any event and move to the To state if
    /// the Predicate returns true.
    pub fn with_predicated_transition(mut self, from_state: impl Into<FromState<TState>>, get_to_state: impl Into<ToState<TEvent, TState, TData>>, event_predicate: impl Fn(&StateTransitionEffectData<TEvent, TState, TData>) -> bool + Send + 'a) -> Self
    {
        self.transitions.push(StateMachineTransition::new(None, Some(Box::new(event_predicate)), from_state.into(), get_to_state.into(), None));
        self
    }

    /// Adds an unnamed Transition to the State Machine definition with a predicate and a Side
    /// Effect. This transition will test the predicate for any event and execute the Side Effect
    /// then move to the To state if the Predicate returns true.
    pub fn with_predicated_transition_effect(mut self, from_state: impl Into<FromState<TState>>, get_to_state: impl Into<ToState<TEvent, TState, TData>>, event_predicate: impl Fn(&StateTransitionEffectData<TEvent, TState, TData>) -> bool + Send + 'a, effect: impl Fn(StateTransitionEffectData<TEvent, TState, TData>) -> Result<(), Box<dyn std::error::Error + Send>> + Send + 'a) -> Self
    {
        self.transitions.push(StateMachineTransition::new(None, Some(Box::new(event_predicate)), from_state.into(), get_to_state.into(), Some(Box::new(effect))));
        self
    }
}

impl <'a, TEvent, TState: PartialEq<TState> + Debug + Clone + Send + Eq + PartialEq + 'a, TData> StateMachineFactory<'a, TEvent, TState, TData>
where TEvent: PartialEq<TEvent> + Sync
{
    /// Adds a named Transition to the State Machine definition whose predicate checks for equality with a
    /// provided Event reference. This is syntactic sugar for `.with_predicated_transition(..)` with
    /// an equality Predicate.
    pub fn with_named_event_transition<'b>(mut self, name: impl Into<String>, event: &'a TEvent, from_state: impl Into<FromState<TState>>, get_to_state: impl Into<ToState<TEvent, TState, TData>>) -> Self
    {
        self.transitions.push(
            StateMachineTransition::new(
                Some(name.into()),
                Some(Box::new(|e| *event == *e.event)),
                from_state.into(),
                get_to_state.into(),
                None
            )
        );
        self
    }

    /// Adds a named Transition with a side effect to the State Machine definition whose predicate checks
    /// for equality with a provided Event reference. This is syntactic sugar for
    /// `.with_predicated_transition(..)` with an equality Predicate.
    pub fn with_named_event_transition_effect(mut self, name: impl Into<String>, event: &'a TEvent, from_state: impl Into<FromState<TState>>, get_to_state: impl Into<ToState<TEvent, TState, TData>>, effect: impl Fn(StateTransitionEffectData<TEvent, TState, TData>) -> Result<(), Box<dyn std::error::Error + Send>> + Send + 'a) -> Self
    {
        self.transitions.push(
            StateMachineTransition::new(
                Some(name.into()),
                Some(Box::new(|e| *event == *e.event)),
                from_state.into(),
                get_to_state.into(),
                Some(Box::new(effect))
            )
        );
        self
    }
    /// Adds an unnamed Transition to the State Machine definition whose predicate checks for
    /// equality with a provided Event reference. This is syntactic sugar for
    /// `.with_predicated_transition(..)` with an equality Predicate.
    pub fn with_event_transition<'b>(mut self, event: &'a TEvent, from_state: impl Into<FromState<TState>>, get_to_state: impl Into<ToState<TEvent, TState, TData>>) -> Self
    {
        self.transitions.push(
            StateMachineTransition::new(
                None,
                Some(Box::new(|e| *event == *e.event)),
                from_state.into(),
                get_to_state.into(),
                None
            )
        );
        self
    }

    /// Adds an unnamed Transition with a side effect to the State Machine definition whose
    /// predicate checks for equality with a provided Event reference. This is syntactic sugar for
    /// `.with_predicated_transition(..)` with an equality Predicate.
    pub fn with_event_transition_effect(mut self, event: &'a TEvent, from_state: impl Into<FromState<TState>>, get_to_state: impl Into<ToState<TEvent, TState, TData>>, effect: impl Fn(StateTransitionEffectData<TEvent, TState, TData>) -> Result<(), Box<dyn std::error::Error + Send>> + Send + 'a) -> Self
    {
        self.transitions.push(
            StateMachineTransition::new(
                None,
                Some(Box::new(|e| *event == *e.event)),
                from_state.into(),
                get_to_state.into(),
                Some(Box::new(effect))
            )
        );
        self
    }
}

/// Basic error type for [StateMachine]
#[derive(Error, Debug)]
pub enum StateMachineError<TState: Debug + Send + Clone + Eq + PartialEq> {
    /// Basic error type for [StateMachine::handle_event]
    #[error("error running effect moving from state {0:?} to {1:?}: {2:?}")]
    EffectError(TState, TState, Box<dyn std::error::Error + Send>)
}

/// Describes a Transition between States, potentially with a Predicate and/or Effect
pub struct StateMachineTransition<'a, TEvent, TState: PartialEq<TState> + Clone + Send + 'a, TData>
{
    name: Option<String>,
    from_state: FromState<TState>,
    get_to_state: ToState<TEvent, TState, TData>,
    event_predicate: Option<Box<dyn Fn(&StateTransitionEffectData<TEvent, TState, TData>) -> bool + Send + 'a>>,
    effect: Option<Box<dyn Fn(StateTransitionEffectData<TEvent, TState, TData>) -> Result<(), Box<dyn std::error::Error + Send>> + Send + 'a>>
}

impl <'a, TEvent, TState: PartialEq<TState> + Clone + Send + 'a, TData> StateMachineTransition<'a, TEvent, TState, TData> {
    fn new(
        name: Option<String>,
        event_predicate: Option<Box<dyn Fn(&StateTransitionEffectData<TEvent, TState, TData>) -> bool + Send + 'a>>,
        from_state: FromState<TState>,
        get_to_state: ToState<TEvent, TState, TData>,
        effect: Option<Box<dyn Fn(StateTransitionEffectData<TEvent, TState, TData>) -> Result<(), Box<dyn std::error::Error + Send>> + Send + 'a>>,
    ) -> Self
    {
        Self {
            name,
            event_predicate,
            from_state,
            get_to_state,
            effect
        }
    }
}

/// Indicates the State or set of States from which a Transition is valid
#[derive(Clone, Eq, PartialEq)]
pub enum FromState<TState: PartialEq<TState> + Clone> {
    /// Indicates that a Transition is valid from any State
    Any,
    /// Indicates that a Transition is valid from any State in the provided Vector
    AnyOf(Vec<TState>),
    /// Indicates that a Transition is valid only from the specified State
    From(TState)
}

impl <TState: PartialEq<TState> + Clone> From<TState> for FromState<TState> {
    fn from(value: TState) -> Self {
        FromState::From(value)
    }
}

/// Indicates how a result State is determined after transitioning
pub enum ToState<TEvent, TState: PartialEq<TState> + Clone + Send, TData> {
    /// Indicates that a Transition should be applied without changing state.
    /// This is a special case, intended for Transitions that want to execute Effects
    /// without causing a state change (e.g. Loggers). Count As Transition should be
    /// set to true for Transitions that should stop execution, or false for Transactions
    /// that should not stop execution.
    Same,
    /// Specifies that a Transition will cause the State Machine to move to the specified State.
    To(TState),
    /// Allows a Transition to provide bespoke logic for determining which State to transition into.
    Calc(Box<dyn Fn(StateTransitionToStateData<TEvent, TState, TData>) -> TState>)
}

impl <TEvent, TState: PartialEq<TState> + Clone + Send, TData> From<TState> for ToState<TEvent, TState, TData> {
    fn from(value: TState) -> Self {
        ToState::<TEvent, TState, TData>::To(value)
    }
}

/// Data passed to a Transition Effect callback.
#[derive(Clone)]
pub struct StateTransitionEffectData<'a, TEvent, TState, TData> {
    /// The name of the transition, if any.
    pub name: &'a Option<String>,
    /// The event causing this transition to occur.
    pub event: &'a TEvent,
    /// The current data associated with the State Machine.
    pub data: &'a TData,
    /// The state that is being transitioned from.
    pub from: &'a TState,
    /// The state that is being transitioned into.
    pub to: &'a TState
}

/// Data passed to a Transition ToState callback.
#[derive(Clone)]
pub struct StateTransitionToStateData<'a, TEvent, TState, TData> {
    /// The event causing this transition to occur.
    pub event: &'a TEvent,
    /// The current data associated with the State Machine.
    pub data: &'a TData,
    /// The state that is being transitioned from.
    pub from: &'a TState,
}

#[cfg(test)]
mod unit_tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use anyhow::{anyhow};
    use thiserror::Error;
    use crate::{StateMachineFactory, StateMachineError};
    use crate::FromState::From;
    use crate::ToState::To;

    #[test]
    fn test_state_machine() {
        #[derive(Eq, PartialEq)]
        enum StateMachineMessage {
            GoToTwo,
            GoToThree
        }

        let go_to_two_happened = AtomicBool::new(false);
        let go_to_three_happened = AtomicBool::new(false);
        let mut sm = StateMachineFactory::new()
            .with_event_transition_effect(
                &StateMachineMessage::GoToTwo,
                1,
                2,
                |_| {
                    go_to_two_happened.store(true, Ordering::SeqCst);
                    Ok(())
                }
            )
            .with_event_transition_effect(
                &StateMachineMessage::GoToThree,
                2,
                3,
                |_| {
                    go_to_three_happened.store(true, Ordering::SeqCst);
                    Ok(())
                }
            ).lock().build(1, ());

        // Assert our starting state is 1
        assert_eq!(1, sm.state);
        // Nothing is going to happen, because no transition is defined for GoToThree from the current state of 1
        assert_eq!(&1, sm.handle_event(StateMachineMessage::GoToThree).expect("unexpected error"));
        // This will transition the state to 2
        assert_eq!(&2, sm.handle_event(StateMachineMessage::GoToTwo).expect("unexpected error"));
        // Assert that the side effect occurred
        assert!(go_to_two_happened.load(Ordering::SeqCst), "effect from GoToTwo did not happen when expected");
        // Assert that we are in state 2
        assert_eq!(2, sm.state);

        // Nothing is going to happen, because no transition is defined for GoToTwo from the current state of 2
        assert_eq!(&2, sm.handle_event(StateMachineMessage::GoToTwo).expect("unexpected error"));
        // This will transition the state to 3
        assert_eq!(&3, sm.handle_event(StateMachineMessage::GoToThree).expect("unexpected error"));
        // Assert that the side effect occurred
        assert!(go_to_three_happened.load(Ordering::SeqCst), "effect from GoToThree did not happen when expected");
        // Assert that we are in state 3
        assert_eq!(3, sm.state);
    }

    #[test]
    fn test_double_transition<'a>() -> anyhow::Result<()> {
        #[derive(Eq, PartialEq)]
        enum StateMachineMessage {
            GoToTwo
        }

        // State here is just an integer
        let factory = StateMachineFactory::new()
            // Evaluate all transitions in a loop
            // until no transition occurs
            .cycle(true)
            // When we receive a GoToTwo event
            // while in state 1, go to state 2
            .with_event_transition(
                &StateMachineMessage::GoToTwo,
                1,
                2
            )
            // When we transition to state 2,
            // immediately transition to state 3
            .with_auto_transition(
                2,
                3
            )
            // Lock the factory object so that
            // we can build a state machine
            .lock();

        // Build the state machine, with an empty () as data
        // (we don't care about data for this example)
        let mut sm = factory.build(1, ());

        // The StateMachine starts in state 1
        assert_eq!(1, sm.state);

        // Handling an event tells us what state we end up in
        match sm.handle_event(StateMachineMessage::GoToTwo) {
            Ok(state) => {
                assert_eq!(3, *state);
            }
            Err(StateMachineError::EffectError(from, to, e)) => {
                return Err(anyhow!("error changing state from {} to {}: {}", from, to, e));
            }
        };

        // Because of the two transitions that we defined,
        // we end up in state 3
        assert_eq!(3, sm.state);
        Ok(())
    }

    #[test]
    fn test_effect_error() -> anyhow::Result<()> {
        #[derive(Eq, PartialEq, Debug)]
        enum StateMachineMessage {
            GoToTwo
        }

        #[derive(Error, Debug)]
        enum TestError {
            #[error("test error")]
            TestError
        }

        let mut sm = StateMachineFactory::new()
            .with_event_transition_effect(
                &StateMachineMessage::GoToTwo,
                From(1),
                To(2),
                |_| {
                    Err(Box::new(TestError::TestError))
                }
            ).lock().build(1, ());

        match sm.handle_event(StateMachineMessage::GoToTwo) {
            Ok(_) => {
                Err(anyhow!("expected an error"))
            },
            Err(StateMachineError::EffectError(from, to, _cause)) => {
                assert_eq!(1, from);
                assert_eq!(2, to);
                Ok(())
            }
        }
    }
}