use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::ci::CiActionKind;
use super::{Action, KeyContext};

/// Lines moved by Ctrl+d / Ctrl+u / PageDown / PageUp.
const PAGE: u16 = 10;

/// Translates a key press into an action. Unknown keys return `None`.
/// The same key can mean different things depending on what is on screen: while typing a
/// filter or a comment, letters are text, not commands.
pub fn action_for_key(key: KeyEvent, context: KeyContext) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl && key.code == KeyCode::Char('c') {
        return Some(Action::Quit);
    }
    match context {
        KeyContext::List => list_key(key, ctrl),
        KeyContext::Filter => filter_key(key, ctrl),
        KeyContext::Detail => detail_key(key, ctrl),
        KeyContext::DetailPipelines => pipelines_key(key, ctrl).or_else(|| detail_key(key, ctrl)),
        KeyContext::CommentEditor => comment_key(key, ctrl),
        KeyContext::ReviewEditor => match key.code {
            KeyCode::Tab => Some(Action::CycleVerdict),
            _ => comment_key(key, ctrl).filter(|a| !matches!(a, Action::SaveDraft)),
        },
        KeyContext::Picker => match key.code {
            KeyCode::Char('j') | KeyCode::Down => Some(Action::SelectNext),
            KeyCode::Char('k') | KeyCode::Up => Some(Action::SelectPrevious),
            KeyCode::Enter => Some(Action::PickerSelect),
            KeyCode::Esc => Some(Action::Back),
            KeyCode::Char('q') => Some(Action::Quit),
            _ => None,
        },
        // Typing filters the repositories, so letters are text here.
        KeyContext::PickerSearch => match key.code {
            KeyCode::Down => Some(Action::SelectNext),
            KeyCode::Up => Some(Action::SelectPrevious),
            KeyCode::Char('n') if ctrl => Some(Action::SelectNext),
            KeyCode::Char('p') if ctrl => Some(Action::SelectPrevious),
            KeyCode::Enter => Some(Action::PickerSelect),
            KeyCode::Esc => Some(Action::Back),
            KeyCode::Backspace => Some(Action::PickerBackspace),
            KeyCode::Char(c) if !ctrl => Some(Action::PickerInput(c)),
            _ => None,
        },
        KeyContext::Branch => branch_key(key, ctrl),
        // Typing filters the branches, so letters are text here.
        KeyContext::BranchBase => match key.code {
            KeyCode::Down => Some(Action::SelectNext),
            KeyCode::Up => Some(Action::SelectPrevious),
            KeyCode::Char('n') if ctrl => Some(Action::SelectNext),
            KeyCode::Char('p') if ctrl => Some(Action::SelectPrevious),
            KeyCode::Enter => Some(Action::PickerSelect),
            KeyCode::Esc => Some(Action::ClosePopup),
            KeyCode::Backspace => Some(Action::PickerBackspace),
            KeyCode::Char(c) if !ctrl => Some(Action::PickerInput(c)),
            _ => None,
        },
        KeyContext::Confirm => match key.code {
            KeyCode::Char('y' | 'Y') => Some(Action::Confirm),
            KeyCode::Char('n' | 'N') | KeyCode::Esc => Some(Action::ClosePopup),
            _ => None,
        },
    }
}

fn list_key(key: KeyEvent, ctrl: bool) -> Option<Action> {
    if ctrl {
        return None;
    }
    match key.code {
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Esc => Some(Action::Back),
        KeyCode::Enter => Some(Action::OpenDetail),
        KeyCode::Char('1') => Some(Action::ShowMergeRequests),
        KeyCode::Char('2') => Some(Action::ShowPipelines),
        KeyCode::Char('3') => Some(Action::ShowBranches),
        // Branches view: compare the selected branch with a branch of your choice.
        KeyCode::Char('b') => Some(Action::PickBase),
        KeyCode::Char('C') => Some(Action::Checkout),
        KeyCode::Char('e') => Some(Action::OpenEditor),
        KeyCode::Char('s') => Some(Action::OpenPicker),
        KeyCode::Char('j') | KeyCode::Down => Some(Action::SelectNext),
        KeyCode::Char('k') | KeyCode::Up => Some(Action::SelectPrevious),
        KeyCode::Char('g') | KeyCode::Home => Some(Action::SelectFirst),
        KeyCode::Char('G') | KeyCode::End => Some(Action::SelectLast),
        KeyCode::Char(']') | KeyCode::Tab => Some(Action::NextTab),
        KeyCode::Char('[') | KeyCode::BackTab => Some(Action::PreviousTab),
        KeyCode::Char('r') => Some(Action::Refresh),
        KeyCode::Char('o') => Some(Action::OpenInBrowser),
        KeyCode::Char('/') => Some(Action::StartFilter),
        _ => None,
    }
}

fn filter_key(key: KeyEvent, ctrl: bool) -> Option<Action> {
    if ctrl {
        return None;
    }
    match key.code {
        KeyCode::Esc => Some(Action::ClearFilter),
        KeyCode::Enter => Some(Action::ApplyFilter),
        KeyCode::Backspace => Some(Action::FilterBackspace),
        KeyCode::Down => Some(Action::SelectNext),
        KeyCode::Up => Some(Action::SelectPrevious),
        KeyCode::Char(c) => Some(Action::FilterInput(c)),
        _ => None,
    }
}

fn detail_key(key: KeyEvent, ctrl: bool) -> Option<Action> {
    if ctrl {
        return match key.code {
            KeyCode::Char('d') => Some(Action::ScrollDown(PAGE)),
            KeyCode::Char('u') => Some(Action::ScrollUp(PAGE)),
            _ => None,
        };
    }
    match key.code {
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Esc => Some(Action::CloseDetail),
        KeyCode::Char(']') | KeyCode::Tab => Some(Action::NextSection),
        KeyCode::Char('[') | KeyCode::BackTab => Some(Action::PreviousSection),
        KeyCode::Char('j') | KeyCode::Down => Some(Action::SelectNext),
        KeyCode::Char('k') | KeyCode::Up => Some(Action::SelectPrevious),
        KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => Some(Action::FocusContent),
        KeyCode::Char('h') | KeyCode::Left => Some(Action::FocusList),
        KeyCode::Char('J') => Some(Action::ScrollDown(1)),
        KeyCode::Char('K') => Some(Action::ScrollUp(1)),
        KeyCode::PageDown | KeyCode::Char(' ') => Some(Action::ScrollDown(PAGE)),
        KeyCode::PageUp => Some(Action::ScrollUp(PAGE)),
        KeyCode::Char('a') => Some(Action::ToggleApprove),
        KeyCode::Char('R') => Some(Action::StartReview),
        KeyCode::Char('d') => Some(Action::DeleteDraft),
        KeyCode::Char('C') => Some(Action::Checkout),
        KeyCode::Char('e') => Some(Action::OpenEditor),
        KeyCode::Char('c') => Some(Action::StartComment),
        KeyCode::Char('m') => Some(Action::StartMerge),
        KeyCode::Char('o') => Some(Action::OpenInBrowser),
        KeyCode::Char('r') => Some(Action::Refresh),
        _ => None,
    }
}

/// Extra keys of the Pipelines section; everything else falls back to `detail_key`.
fn pipelines_key(key: KeyEvent, ctrl: bool) -> Option<Action> {
    if ctrl {
        return match key.code {
            KeyCode::Char('r') => Some(Action::Ci(CiActionKind::RetryPipeline)),
            _ => None,
        };
    }
    match key.code {
        KeyCode::Enter => Some(Action::OpenJobLog),
        KeyCode::Char('g') | KeyCode::Home => Some(Action::ScrollToTop),
        KeyCode::Char('G') | KeyCode::End => Some(Action::ScrollToBottom),
        KeyCode::Char('f') => Some(Action::ToggleFollow),
        KeyCode::Char('R') => Some(Action::Ci(CiActionKind::RetryJob)),
        KeyCode::Char('C') => Some(Action::Ci(CiActionKind::CancelJob)),
        KeyCode::Char('X') => Some(Action::Ci(CiActionKind::CancelPipeline)),
        KeyCode::Char('P') => Some(Action::OlderPipeline),
        KeyCode::Char('p') => Some(Action::NewerPipeline),
        _ => None,
    }
}

/// A branch compared with the default branch. Viewing keys only: nothing here writes.
fn branch_key(key: KeyEvent, ctrl: bool) -> Option<Action> {
    if ctrl {
        return match key.code {
            KeyCode::Char('d') => Some(Action::ScrollDown(PAGE)),
            KeyCode::Char('u') => Some(Action::ScrollUp(PAGE)),
            _ => None,
        };
    }
    match key.code {
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Esc => Some(Action::CloseDetail),
        KeyCode::Char('j') | KeyCode::Down => Some(Action::SelectNext),
        KeyCode::Char('k') | KeyCode::Up => Some(Action::SelectPrevious),
        KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => Some(Action::FocusContent),
        KeyCode::Char('h') | KeyCode::Left => Some(Action::FocusList),
        KeyCode::Char('J') => Some(Action::ScrollDown(1)),
        KeyCode::Char('K') => Some(Action::ScrollUp(1)),
        KeyCode::PageDown | KeyCode::Char(' ') => Some(Action::ScrollDown(PAGE)),
        KeyCode::PageUp => Some(Action::ScrollUp(PAGE)),
        KeyCode::Char('o') => Some(Action::OpenInBrowser),
        KeyCode::Char('r') => Some(Action::Refresh),
        KeyCode::Char('b') => Some(Action::PickBase),
        KeyCode::Char('C') => Some(Action::Checkout),
        KeyCode::Char('e') => Some(Action::OpenEditor),
        _ => None,
    }
}

fn comment_key(key: KeyEvent, ctrl: bool) -> Option<Action> {
    if ctrl {
        // Raw mode turns off terminal flow control, so Ctrl+S reaches us instead of
        // freezing the terminal.
        return match key.code {
            KeyCode::Char('s') => Some(Action::SubmitComment),
            KeyCode::Char('d') => Some(Action::SaveDraft),
            _ => None,
        };
    }
    match key.code {
        KeyCode::Esc => Some(Action::ClosePopup),
        KeyCode::Enter => Some(Action::CommentNewline),
        KeyCode::Backspace => Some(Action::CommentBackspace),
        KeyCode::Char(c) => Some(Action::CommentInput(c)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn vim_keys_and_arrows_on_the_list() {
        assert!(matches!(
            action_for_key(key(KeyCode::Char('j')), KeyContext::List),
            Some(Action::SelectNext)
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Up), KeyContext::List),
            Some(Action::SelectPrevious)
        ));
        // Shift+g arrives as an uppercase 'G' (often with the SHIFT modifier set).
        let shift_g = KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert!(matches!(
            action_for_key(shift_g, KeyContext::List),
            Some(Action::SelectLast)
        ));
        // Shift+Tab arrives as BackTab.
        let back_tab = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
        assert!(matches!(
            action_for_key(back_tab, KeyContext::List),
            Some(Action::PreviousTab)
        ));
    }

    #[test]
    fn enter_reads_the_diff_but_opens_a_log_on_the_pipelines_section() {
        assert!(matches!(
            action_for_key(key(KeyCode::Enter), KeyContext::Detail),
            Some(Action::FocusContent)
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Char('h')), KeyContext::Branch),
            Some(Action::FocusList)
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Enter), KeyContext::DetailPipelines),
            Some(Action::OpenJobLog)
        ));
    }

    #[test]
    fn letters_are_text_while_typing() {
        assert!(matches!(
            action_for_key(key(KeyCode::Char('q')), KeyContext::Filter),
            Some(Action::FilterInput('q'))
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Char('m')), KeyContext::CommentEditor),
            Some(Action::CommentInput('m'))
        ));
        assert!(matches!(
            action_for_key(ctrl('s'), KeyContext::CommentEditor),
            Some(Action::SubmitComment)
        ));
    }

    #[test]
    fn esc_depends_on_the_context() {
        let esc = key(KeyCode::Esc);
        assert!(matches!(
            action_for_key(esc, KeyContext::List),
            Some(Action::Back)
        ));
        assert!(matches!(
            action_for_key(esc, KeyContext::Filter),
            Some(Action::ClearFilter)
        ));
        assert!(matches!(
            action_for_key(esc, KeyContext::Detail),
            Some(Action::CloseDetail)
        ));
        assert!(matches!(
            action_for_key(esc, KeyContext::CommentEditor),
            Some(Action::ClosePopup)
        ));
    }

    #[test]
    fn pipeline_keys_extend_the_detail_keys() {
        let ctx = KeyContext::DetailPipelines;
        assert!(matches!(
            action_for_key(key(KeyCode::Char('R')), ctx),
            Some(Action::Ci(CiActionKind::RetryJob))
        ));
        assert!(matches!(
            action_for_key(ctrl('r'), ctx),
            Some(Action::Ci(CiActionKind::RetryPipeline))
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Enter), ctx),
            Some(Action::OpenJobLog)
        ));
        // Keys of the other sections still work here.
        assert!(matches!(
            action_for_key(key(KeyCode::Char('c')), ctx),
            Some(Action::StartComment)
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Esc), ctx),
            Some(Action::CloseDetail)
        ));
        // `R` retries a job here, and finishes the review elsewhere.
        assert!(matches!(
            action_for_key(key(KeyCode::Char('R')), ctx),
            Some(Action::Ci(CiActionKind::RetryJob))
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Char('R')), KeyContext::Detail),
            Some(Action::StartReview)
        ));
        // Other pipeline keys do nothing elsewhere.
        assert!(action_for_key(key(KeyCode::Char('X')), KeyContext::Detail).is_none());
    }

    #[test]
    fn picker_keys() {
        assert!(matches!(
            action_for_key(key(KeyCode::Char('s')), KeyContext::List),
            Some(Action::OpenPicker)
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Char('j')), KeyContext::Picker),
            Some(Action::SelectNext)
        ));
        // On the repository step, `j` is part of a name being typed.
        assert!(matches!(
            action_for_key(key(KeyCode::Char('j')), KeyContext::PickerSearch),
            Some(Action::PickerInput('j'))
        ));
        assert!(matches!(
            action_for_key(ctrl('n'), KeyContext::PickerSearch),
            Some(Action::SelectNext)
        ));
    }

    #[test]
    fn merge_needs_an_explicit_yes() {
        assert!(matches!(
            action_for_key(key(KeyCode::Char('y')), KeyContext::Confirm),
            Some(Action::Confirm)
        ));
        // Enter does not confirm: a merge should never happen by accident.
        assert!(action_for_key(key(KeyCode::Enter), KeyContext::Confirm).is_none());
    }

    #[test]
    fn ctrl_c_always_quits() {
        for context in [
            KeyContext::List,
            KeyContext::Filter,
            KeyContext::Detail,
            KeyContext::DetailPipelines,
            KeyContext::CommentEditor,
            KeyContext::Confirm,
            KeyContext::Picker,
            KeyContext::PickerSearch,
        ] {
            assert!(matches!(
                action_for_key(ctrl('c'), context),
                Some(Action::Quit)
            ));
        }
        assert!(action_for_key(key(KeyCode::Char('c')), KeyContext::List).is_none());
    }
}
