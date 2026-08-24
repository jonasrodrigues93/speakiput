//! Portable dictation orchestration and state.

mod insertion;
mod state;

pub use insertion::{LlmInsertion, prepare_llm_insertion};
pub use state::{Action, StateMachine, Transition, TransitionError};
