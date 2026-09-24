//! Platform ownership keeps OS identifiers live until the final tree signal.
#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(crate) use unix::ContainedChild;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::ContainedChild;

#[cfg(any(target_os = "macos", test))]
fn macos_members_are_dead<I, F>(members: I, mut is_zombie: F) -> bool
where
    I: IntoIterator<Item = i32>,
    F: FnMut(i32) -> Option<bool>,
{
    members.into_iter().all(|pid| {
        if pid == 0 {
            return true;
        }
        is_zombie(pid).unwrap_or(false)
    })
}

#[cfg(test)]
mod macos_group_tests {
    use super::macos_members_are_dead;

    #[test]
    fn live_group_leader_keeps_a_leader_only_group_alive() {
        let leader_pid = 412;
        let mut inspected = Vec::new();

        let group_is_dead = macos_members_are_dead([leader_pid], |pid| {
            inspected.push(pid);
            Some(false)
        });

        assert!(!group_is_dead);
        assert_eq!(inspected, [leader_pid]);
    }

    #[test]
    fn unknown_member_status_is_conservatively_live() {
        assert!(!macos_members_are_dead([412], |_| None));
    }
}
