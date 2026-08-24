// Adapted from whisrs src/state.rs at commit
// 28139bd8c4ff17e8d0fd156a0d903a7baa423d48. Copyright (c) 2025-present
// Yosif Kitaneh, used under the MIT License; see THIRD_PARTY_LICENSES.md.

use speakiput_contract::DictationState;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Start {
        session_id: Uuid,
    },
    Stop {
        session_id: Uuid,
    },
    TranscriptionComplete {
        session_id: Uuid,
        post_process: bool,
        inject: bool,
    },
    PostProcessingComplete {
        session_id: Uuid,
        inject: bool,
    },
    InjectionComplete {
        session_id: Uuid,
    },
    Cancel {
        session_id: Uuid,
    },
    Fail {
        session_id: Uuid,
    },
    Recover,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    pub previous: DictationState,
    pub current: DictationState,
    pub session_id: Option<Uuid>,
    pub terminal: bool,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TransitionError {
    #[error("action {action:?} is invalid while backend is {state:?}")]
    InvalidState {
        state: DictationState,
        action: Action,
    },
    #[error("action is for session {received}, but active session is {active}")]
    StaleSession { active: Uuid, received: Uuid },
}

#[derive(Debug)]
pub struct StateMachine {
    state: DictationState,
    active_session_id: Option<Uuid>,
}

impl Default for StateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl StateMachine {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: DictationState::Idle,
            active_session_id: None,
        }
    }

    #[must_use]
    pub const fn state(&self) -> DictationState {
        self.state
    }

    #[must_use]
    pub const fn active_session_id(&self) -> Option<Uuid> {
        self.active_session_id
    }

    pub fn transition(&mut self, action: Action) -> Result<Transition, TransitionError> {
        let previous = self.state;
        let (current, session_id, terminal) = match action {
            Action::Start { session_id } if previous == DictationState::Idle => {
                self.active_session_id = Some(session_id);
                (DictationState::Recording, Some(session_id), false)
            }
            Action::Stop { session_id } if previous == DictationState::Recording => {
                self.require_active(session_id)?;
                (DictationState::Transcribing, Some(session_id), false)
            }
            Action::TranscriptionComplete {
                session_id,
                post_process: true,
                ..
            } if previous == DictationState::Transcribing => {
                self.require_active(session_id)?;
                (DictationState::PostProcessing, Some(session_id), false)
            }
            Action::TranscriptionComplete {
                session_id,
                post_process: false,
                inject: true,
            } if previous == DictationState::Transcribing => {
                self.require_active(session_id)?;
                (DictationState::Injecting, Some(session_id), false)
            }
            Action::TranscriptionComplete {
                session_id,
                post_process: false,
                inject: false,
            } if previous == DictationState::Transcribing => {
                self.require_active(session_id)?;
                self.active_session_id = None;
                (DictationState::Idle, Some(session_id), true)
            }
            Action::PostProcessingComplete {
                session_id,
                inject: true,
            } if previous == DictationState::PostProcessing => {
                self.require_active(session_id)?;
                (DictationState::Injecting, Some(session_id), false)
            }
            Action::PostProcessingComplete {
                session_id,
                inject: false,
            } if previous == DictationState::PostProcessing => {
                self.require_active(session_id)?;
                self.active_session_id = None;
                (DictationState::Idle, Some(session_id), true)
            }
            Action::InjectionComplete { session_id } if previous == DictationState::Injecting => {
                self.require_active(session_id)?;
                self.active_session_id = None;
                (DictationState::Idle, Some(session_id), true)
            }
            Action::Cancel { session_id } if previous == DictationState::Recording => {
                self.require_active(session_id)?;
                self.active_session_id = None;
                (DictationState::Idle, Some(session_id), true)
            }
            Action::Fail { session_id }
                if matches!(
                    previous,
                    DictationState::Recording
                        | DictationState::Transcribing
                        | DictationState::PostProcessing
                        | DictationState::Injecting
                ) =>
            {
                self.require_active(session_id)?;
                (DictationState::Error, Some(session_id), true)
            }
            Action::Recover if previous == DictationState::Error => {
                self.active_session_id = None;
                (DictationState::Idle, None, false)
            }
            Action::Shutdown
                if matches!(previous, DictationState::Idle | DictationState::Error) =>
            {
                self.active_session_id = None;
                (DictationState::ShuttingDown, None, false)
            }
            _ => {
                return Err(TransitionError::InvalidState {
                    state: previous,
                    action,
                });
            }
        };
        debug_assert!(previous.can_transition_to(current));
        self.state = current;
        Ok(Transition {
            previous,
            current,
            session_id,
            terminal,
        })
    }

    fn require_active(&self, received: Uuid) -> Result<(), TransitionError> {
        match self.active_session_id {
            Some(active) if active == received => Ok(()),
            Some(active) => Err(TransitionError::StaleSession { active, received }),
            None => Err(TransitionError::InvalidState {
                state: self.state,
                action: Action::Cancel {
                    session_id: received,
                },
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_cycle_with_post_processing_and_injection() {
        let session_id = Uuid::new_v4();
        let mut state = StateMachine::new();
        assert_eq!(
            state
                .transition(Action::Start { session_id })
                .unwrap()
                .current,
            DictationState::Recording
        );
        assert_eq!(
            state
                .transition(Action::Stop { session_id })
                .unwrap()
                .current,
            DictationState::Transcribing
        );
        assert_eq!(
            state
                .transition(Action::TranscriptionComplete {
                    session_id,
                    post_process: true,
                    inject: true,
                })
                .unwrap()
                .current,
            DictationState::PostProcessing
        );
        state
            .transition(Action::PostProcessingComplete {
                session_id,
                inject: true,
            })
            .unwrap();
        let final_transition = state
            .transition(Action::InjectionComplete { session_id })
            .unwrap();
        assert_eq!(final_transition.current, DictationState::Idle);
        assert!(final_transition.terminal);
        assert_eq!(state.active_session_id(), None);
    }

    #[test]
    fn explicit_start_rejects_duplicate_start() {
        let mut state = StateMachine::new();
        state
            .transition(Action::Start {
                session_id: Uuid::new_v4(),
            })
            .unwrap();
        let error = state
            .transition(Action::Start {
                session_id: Uuid::new_v4(),
            })
            .unwrap_err();
        assert!(matches!(error, TransitionError::InvalidState { .. }));
        assert_eq!(state.state(), DictationState::Recording);
    }

    #[test]
    fn late_completion_after_cancel_cannot_move_new_session() {
        let old_session = Uuid::new_v4();
        let new_session = Uuid::new_v4();
        let mut state = StateMachine::new();
        state
            .transition(Action::Start {
                session_id: old_session,
            })
            .unwrap();
        state
            .transition(Action::Cancel {
                session_id: old_session,
            })
            .unwrap();
        state
            .transition(Action::Start {
                session_id: new_session,
            })
            .unwrap();
        let error = state
            .transition(Action::TranscriptionComplete {
                session_id: old_session,
                post_process: false,
                inject: false,
            })
            .unwrap_err();
        assert!(matches!(error, TransitionError::InvalidState { .. }));
        assert_eq!(state.active_session_id(), Some(new_session));
        assert_eq!(state.state(), DictationState::Recording);
    }

    #[test]
    fn stale_stop_is_rejected_without_mutation() {
        let active = Uuid::new_v4();
        let mut state = StateMachine::new();
        state
            .transition(Action::Start { session_id: active })
            .unwrap();
        let error = state
            .transition(Action::Stop {
                session_id: Uuid::new_v4(),
            })
            .unwrap_err();
        assert!(matches!(error, TransitionError::StaleSession { .. }));
        assert_eq!(state.state(), DictationState::Recording);
    }

    #[test]
    fn failure_is_terminal_then_recovers_to_idle() {
        let session_id = Uuid::new_v4();
        let mut state = StateMachine::new();
        state.transition(Action::Start { session_id }).unwrap();
        assert!(
            state
                .transition(Action::Fail { session_id })
                .unwrap()
                .terminal
        );
        assert_eq!(state.state(), DictationState::Error);
        state.transition(Action::Recover).unwrap();
        assert_eq!(state.state(), DictationState::Idle);
        assert_eq!(state.active_session_id(), None);
    }
}
