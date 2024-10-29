#[cfg(test)]
mod calculator_tests {
    use std::sync::atomic::Ordering::SeqCst;
    use atomic_float::AtomicF64;
    use statement::FromState::{Any, AnyOf};
    use statement::{StateMachineError, StateMachineFactory, StateTransitionEffectData};
    use statement::ToState::Same;

    struct CalcData {
        pub input_value: AtomicF64,
        pub stored_value: AtomicF64,
    }

    #[derive(Copy, Clone, Eq, PartialEq, Debug)]
    enum States {
        Idle,
        Adding,
        Subtracting,
        Multiplying,
        Dividing
    }

    #[test]
    fn calculator_test() -> Result<(), StateMachineError<States>> {
        #[derive(Copy, Clone, Eq, PartialEq, Debug)]
        enum Events {
            Digit { digit: u8 },
            Add,
            Subtract,
            Multiply,
            Divide,
            Equals
        }

        impl Events {
            fn is_digit(&self) -> bool {
                if let Events::Digit { digit: _ } = self { true } else { false }
            }
        }

        let mut init_data = CalcData {
            input_value: AtomicF64::new(0f64),
            stored_value: AtomicF64::new(0f64)
        };

        let mut sm = StateMachineFactory::<Events, States, &CalcData>::new()
            // This is an example of a logger that runs before any other transition, but doesn't
            // do anything in terms of state transitions itself.
            .with_transition_effect(
                Any,
                Same,
                |d| {
                    print!("user sent {:?} event", d.event);
                    Ok(())
                })
            .with_predicated_transition_effect(
                Any,
                Same,
                |d| d.event.is_digit(),
                |d| {
                    if let Events::Digit { digit } = d.event {
                        append_digit(d.data, digit.clone());
                    }
                    Ok(())
                })
            .with_predicated_transition_effect(
                AnyOf(vec![States::Adding, States::Subtracting, States::Multiplying, States::Dividing]),
                States::Idle,
                |d| {
                    match d.event {
                        Events::Add | Events::Subtract | Events::Multiply | Events::Divide | Events::Equals => true,
                        _ => false
                    }
                },
                |d| {
                    apply_function(d);
                    Ok(())
                })
            .with_event_transition_effect(&Events::Add, States::Idle, States::Adding, |d| {
                swap(d.data);
                Ok(())
            })
            .with_event_transition_effect(&Events::Subtract, States::Idle, States::Subtracting, |d| {
                swap(d.data);
                Ok(())
            })
            .with_event_transition_effect(&Events::Multiply, States::Idle, States::Multiplying, |d| {
                swap(d.data);
                Ok(())
            })
            .with_event_transition_effect(&Events::Divide, States::Idle, States::Dividing, |d| {
                swap(d.data);
                Ok(())
            })
            // This is an example of a logger that runs after any other transition, but doesn't
            // do anything in terms of state transitions itself. It continues the log lines from
            // the earlier logger
            .with_transition_effect(
                Any,
                Same,
                |d| {
                    println!(", input value is {}, stored value is {}", d.data.input_value.load(SeqCst), d.data.stored_value.load(SeqCst));
                    Ok(())
                })
            .lock().build(States::Idle, &mut init_data);

        sm.handle_event(Events::Digit {digit: 2})?;
        sm.handle_event(Events::Add)?;
        sm.handle_event(Events::Digit {digit: 0})?;
        sm.handle_event(Events::Subtract)?;
        sm.handle_event(Events::Digit {digit: 1})?;
        sm.handle_event(Events::Multiply)?;
        sm.handle_event(Events::Digit {digit: 1})?;
        sm.handle_event(Events::Digit {digit: 2})?;
        sm.handle_event(Events::Digit {digit: 6})?;
        sm.handle_event(Events::Divide)?;
        sm.handle_event(Events::Digit {digit: 3})?;
        sm.handle_event(Events::Equals)?;

        assert_eq!(42f64, sm.data.input_value.load(SeqCst));

        return Ok(());

        fn append_digit(d: &CalcData, b: u8) {
            let input_value_current = d.input_value.load(SeqCst);
            d.input_value.store(input_value_current * 10f64 + b as f64, SeqCst);
        }
        fn swap(d: &CalcData) {
            let old_input_value = d.input_value.load(SeqCst);
            d.stored_value.store(old_input_value, SeqCst);
            d.input_value.store(0f64, SeqCst);
        }
        fn apply_function(arg: StateTransitionEffectData<Events, States, &CalcData>) {
            let old_stored_value = arg.data.stored_value.load(SeqCst);
            let old_input_value = arg.data.input_value.load(SeqCst);
            match arg.from {
                States::Adding => {
                    arg.data.input_value.store(old_stored_value + old_input_value, SeqCst);
                }
                States::Subtracting => {
                    arg.data.input_value.store(old_stored_value - old_input_value, SeqCst);
                }
                States::Multiplying => {
                    arg.data.input_value.store(old_stored_value * old_input_value, SeqCst);
                }
                States::Dividing => {
                    arg.data.input_value.store(old_stored_value / old_input_value, SeqCst);
                }
                States::Idle => {}
            }
        }
    }

}