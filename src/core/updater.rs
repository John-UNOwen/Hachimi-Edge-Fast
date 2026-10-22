use std::{sync::{Arc, Mutex}, time::Duration};

use rust_i18n::t;
use serde::Deserialize;

use crate::core::{gui::{HachimiUpdateWindow, NotificationGuard}, hachimi::{GITHUB_API, REPO_PATH}, http, Error, Gui};

const UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Default)]
pub struct Updater {
    update_check_mutex: Mutex<()>,
}

impl Updater {
    pub fn check_for_updates(self: Arc<Self>, callback: fn(bool)) {
        std::thread::spawn(move || {
            match self.check_for_updates_internal() {
                Ok(v) => callback(v),
                Err(e) => error!("{}", e)
            }
        });
    }

    fn check_for_updates_internal(&self) -> Result<bool, Error> {
        // Prevent multiple update checks running at the same time
        let Ok(_guard) = self.update_check_mutex.try_lock() else {
            return Ok(false);
        };

        let checking_notif_id = if let Some(mutex) = Gui::instance() {
            Some(mutex.lock().unwrap().show_persistent_notification(&t!("notification.checking_for_updates")))
        } else {
            None
        };
        let _guard = checking_notif_id.map(NotificationGuard);

        // This fork's own releases, and nothing else. Upstream's release carries an installer that
        // would overwrite this build with one holding none of the speed options this fork exists for,
        // and this fork has no Codeberg copy to fall back to, so there is no second repository here.
        //
        // The list, not `/releases/latest`: GitHub's "latest" skips prereleases, and this fork shipped
        // `v0.32.0-fork.1` and `.2` as prereleases. Through that door a prerelease would be invisible
        // and an older release would be reported as the newest one.
        let releases = http::get_json_with_timeout::<Vec<Release>>(
            &format!("{}/{}/releases?per_page=10", GITHUB_API, REPO_PATH), UPDATE_CHECK_TIMEOUT
        )?;

        let Some(latest) = pick_release(releases) else {
            if let Some(mutex) = Gui::instance() {
                mutex.lock().unwrap().show_notification(&t!("notification.no_updates"));
            }
            return Ok(false);
        };

        if !self.release_offers_an_update(&latest)? {
            if let Some(mutex) = Gui::instance() {
                mutex.lock().unwrap().show_notification(&t!("notification.no_updates"));
            }
            return Ok(false);
        }

        // What a fork release carries is `cri_mana_vpx.dll` for the player to put in the game root
        // with the game closed (C4; tools/deploy-game.ps1 does the same thing on the build machine).
        // This mod downloads nothing and runs nothing it fetched, which is the half of C26 that needs
        // no signing decision. The answer to "a newer build exists" is the page that names it, opened
        // through the same door the About tab's link buttons use (on Windows that is the game's own
        // opener or this fork's in-game webview, both guarded against a null target).
        let url = latest.release_url();
        if let Some(mutex) = Gui::instance() {
            mutex.lock().unwrap().show_window(Box::new(HachimiUpdateWindow::new(
                &t!("update_prompt_dialog.title"),
                &t!("update_prompt_dialog.release_content", version = &latest.tag_name),
                &latest.body,
                move |ok| {
                    if !ok { return; }
                    use crate::il2cpp::{ext::StringExt, hook::UnityEngine_CoreModule::Application};
                    Application::OpenURL(url.to_il2cpp_string());
                }
            )));
        }

        Ok(true)
    }

    /// Whether the release named by `latest` is worth telling the player about.
    ///
    /// A version number alone cannot answer this for this fork: `v0.32.0-fork.1`, `.2` and `.3` were
    /// all published with the Cargo version still 0.32.0, so three releases read as the same version
    /// as the build they replaced. When the numbers tie, the repository answers: GitHub's compare
    /// endpoint says whether the tag is ahead of the commit this build was stamped from. Measured on
    /// 2026-10-18 against this fork: `compare/4e1b0f7...v0.32.0-fork.3` answered `behind`,
    /// `ahead_by: 0`, `behind_by: 20`, which is a release older than the running build.
    fn release_offers_an_update(&self, latest: &Release) -> Result<bool, Error> {
        match number_order(&latest.tag_name, env!("CARGO_PKG_VERSION")) {
            NumberOrder::Greater => return Ok(true),
            NumberOrder::Less => return Ok(false),
            NumberOrder::Equal | NumberOrder::Unreadable => {}
        }

        let Some(our_commit) = build_commit() else {
            // No commit in the stamp means nothing to rank the tag against. Nothing is installed
            // either way, so the release page is shown rather than the release swallowed.
            warn!("Update check: this build's version stamp carries no commit to compare against");
            return Ok(true);
        };

        let url = format!("{}/{}/compare/{}...{}", GITHUB_API, REPO_PATH, our_commit, latest.tag_name);
        match http::get_json_with_timeout::<CompareStatus>(&url, UPDATE_CHECK_TIMEOUT) {
            Ok(status) => {
                let newer = compare_status_is_newer(&status.status);
                if !newer {
                    debug!("Update check: {} is {} of the commit this build was built from",
                           latest.tag_name, status.status);
                }
                Ok(newer)
            }
            Err(e) => {
                // A tag this build cannot place is a question for the player, not a reason to say
                // nothing: the page names what the release carries and they can read it.
                warn!("Update check: cannot compare {} with commit {}: {}", latest.tag_name, our_commit, e);
                Ok(true)
            }
        }
    }
}

#[derive(Deserialize)]
pub struct Release {
    // STUB
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    // The release page's own notes, which GitHub answers beside the tag in the same list this check
    // already reads. A release made without notes leaves it empty and the dialog says so.
    #[serde(default)]
    body: String
}

/// The release to answer about: the highest version among what the fork has published, keeping GitHub's
/// newest-first order when two tags carry the same version (`v0.32.0-fork.1` and `v0.32.0-fork.3` both
/// number 0.32.0, and the later one is the first in that list). A tag with no number in it ranks below
/// one that does, so a stray tag cannot outrank a real release. Drafts are dropped: they are not
/// published, and an unauthenticated check cannot see them anyway.
fn pick_release(mut releases: Vec<Release>) -> Option<Release> {
    releases.retain(|release| !release.draft);
    releases.sort_by(|a, b| version_rank(&b.tag_name).cmp(&version_rank(&a.tag_name)));
    releases.into_iter().next()
}

impl Release {
    /// The page that names this release and what it carries. GitHub answers with its own URL; the tag
    /// is spelled out as a fallback so the answer is still a release page, never a download.
    fn release_url(&self) -> String {
        if self.html_url.is_empty() {
            format!("https://github.com/{}/releases/tag/{}", REPO_PATH, self.tag_name)
        } else {
            self.html_url.clone()
        }
    }
}

#[derive(Deserialize)]
struct CompareStatus {
    // STUB: GitHub's compare endpoint answers `ahead`, `behind`, `identical` or `diverged` for the
    // head side against the base side.
    status: String
}

/// Where a release tag sits against this build's version, by number only.
#[derive(Debug, PartialEq, Eq)]
enum NumberOrder {
    Greater,
    Equal,
    Less,
    Unreadable
}

fn number_order(tag: &str, current: &str) -> NumberOrder {
    let (Some(release), Some(here)) = (version_parts(tag), version_parts(current)) else {
        return NumberOrder::Unreadable;
    };

    for depth in 0..here.len().max(release.len()) {
        let running = *here.get(depth).unwrap_or(&0);
        let offered = *release.get(depth).unwrap_or(&0);
        if offered != running {
            return if offered > running { NumberOrder::Greater } else { NumberOrder::Less };
        }
    }

    NumberOrder::Equal
}

/// `v0.33.0-fork.3`, `0.33.0` and `0.33.0+beta` all read as `[0, 33, 0]`. What follows the first `-`
/// or `+` is dropped because it names a fork build number or a tag name, not a place in a version
/// order. Upstream's check compared the two strings, which reads every `-fork` tag as new.
fn version_parts(text: &str) -> Option<Vec<u64>> {
    let head = text.trim().trim_start_matches('v').split(['-', '+']).next()?;
    if head.is_empty() {
        return None;
    }
    head.split('.').map(|part| part.parse::<u64>().ok()).collect()
}

/// How this fork's own releases rank against each other: the version number first, then the `-fork.N`
/// suffix, so `v0.33.1-fork.2` outranks `v0.33.1-fork.1` and both outrank a plain `v0.33.1` carrying
/// the same number. A tag with no suffix reads fork number 0, and an unreadable version ranks below
/// every readable one. Only releases rank this way: the running build carries no fork number in its
/// stamp, so a tie against a local build still goes to the compare endpoint in
/// `release_offers_an_update`.
fn version_rank(text: &str) -> (Vec<u64>, u64) {
    let number = version_parts(text).unwrap_or_default();
    let tail = text.trim().trim_start_matches('v').split(['-', '+']).nth(1).unwrap_or("");
    let fork = tail.strip_prefix("fork").and_then(|rest| rest.strip_prefix('.'))
        .and_then(|digits| digits.parse::<u64>().ok()).unwrap_or(0);
    (number, fork)
}

/// The commit this build was stamped from, read out of the string `build.rs` writes into
/// `HACHIMI_DISPLAY_VERSION`: `v0.33.0-4e1b0f7`, with `-dirty` appended when the tree was uncommitted.
/// None when the stamp carries no commit (a build made without git).
fn build_commit() -> Option<&'static str> {
    parse_build_commit(env!("HACHIMI_DISPLAY_VERSION"))
}

fn parse_build_commit(stamp: &str) -> Option<&str> {
    let candidate = stamp.trim().trim_start_matches('v').split('-').nth(1)?;
    if candidate.len() >= 7 && candidate.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)) {
        Some(candidate)
    } else {
        None
    }
}

/// Read GitHub's answer as a decision. `diverged` counts as an update because the release does hold
/// commits this build does not, even when the build also holds commits the release never saw.
fn compare_status_is_newer(status: &str) -> bool {
    match status.trim().to_ascii_lowercase().as_str() {
        "behind" | "identical" => false,
        "ahead" | "diverged" => true,
        other => {
            warn!("Update check: unknown compare status {:?}", other);
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(tag: &str) -> Release {
        Release { tag_name: tag.to_string(), html_url: String::new(), draft: false, body: String::new() }
    }

    #[test]
    fn the_update_source_is_this_fork() {
        // A check aimed at upstream would offer a build that replaces this one and drops every speed
        // option in it, so the repository this answers to is part of what the tests hold open.
        assert_eq!(REPO_PATH, "John-UNOwen/Hachimi-Edge-Fast");
    }

    #[test]
    fn a_fork_suffix_is_not_a_different_version() {
        assert_eq!(number_order("v0.33.0-fork.1", "0.33.0"), NumberOrder::Equal);
        assert_eq!(number_order("v0.33.0", "0.33.0"), NumberOrder::Equal);
        assert_eq!(number_order("0.33.0", "0.33.0"), NumberOrder::Equal);
        assert_eq!(number_order("v0.32.0-fork.3", "0.32.0"), NumberOrder::Equal);
    }

    #[test]
    fn only_a_higher_number_is_greater() {
        assert_eq!(number_order("v0.34.0-fork.1", "0.33.0"), NumberOrder::Greater);
        assert_eq!(number_order("v0.33.1", "0.33.0"), NumberOrder::Greater);
        assert_eq!(number_order("v0.33.10", "0.33.9"), NumberOrder::Greater);
        assert_eq!(number_order("v0.32.0-fork.3", "0.33.0"), NumberOrder::Less);
        assert_eq!(number_order("v0.33.0", "0.33.1"), NumberOrder::Less);
    }

    #[test]
    fn a_tag_it_cannot_number_is_called_unreadable() {
        assert_eq!(number_order("nightly", "0.33.0"), NumberOrder::Unreadable);
        assert_eq!(number_order("v", "0.33.0"), NumberOrder::Unreadable);
        assert_eq!(number_order("v0.33.x", "0.33.0"), NumberOrder::Unreadable);
    }

    #[test]
    fn the_commit_comes_out_of_the_version_stamp() {
        assert_eq!(parse_build_commit("v0.33.0-4e1b0f7"), Some("4e1b0f7"));
        assert_eq!(parse_build_commit("v0.33.0-4e1b0f7-dirty"), Some("4e1b0f7"));
        assert_eq!(parse_build_commit("v0.33.0"), None);
        assert_eq!(parse_build_commit("v0.33.0-dirty"), None);
    }

    #[test]
    fn a_release_older_than_this_build_is_not_an_update() {
        // The answer GitHub gave for this fork on 2026-10-18, for `4e1b0f7...v0.32.0-fork.3`.
        assert!(!compare_status_is_newer("behind"));
        assert!(!compare_status_is_newer("identical"));
        assert!(compare_status_is_newer("ahead"));
        assert!(compare_status_is_newer("diverged"));
        assert!(!compare_status_is_newer("Behind"));
        assert!(compare_status_is_newer("AHEAD"));
        // An answer this fork has never seen is a question for the player, not a silence.
        assert!(compare_status_is_newer("something new"));
    }

    #[test]
    fn the_fallback_page_names_the_tag() {
        let release = listed("v0.33.0-fork.1");
        assert_eq!(
            release.release_url(),
            format!("https://github.com/{}/releases/tag/v0.33.0-fork.1", REPO_PATH)
        );

        let with_page = Release {
            tag_name: "v0.33.0-fork.1".to_string(),
            html_url: "https://example.test/release".to_string(),
            draft: false,
            body: String::new()
        };
        assert_eq!(with_page.release_url(), "https://example.test/release");
    }

    #[test]
    fn a_prerelease_is_a_release_a_player_can_install() {
        // v0.32.0-fork.1 and .2 were prereleases and were the builds this fork actually handed out,
        // so the check reads the release list instead of GitHub's prerelease-free "latest".
        let releases = vec![
            listed("v0.32.0-fork.2"),
            listed("v0.32.0-fork.1"),
        ];
        assert_eq!(pick_release(releases).map(|r| r.tag_name), Some("v0.32.0-fork.2".to_string()));
    }

    #[test]
    fn the_highest_version_wins_even_when_it_is_listed_last() {
        let releases = vec![
            listed("v0.32.0-fork.3"),
            listed("v0.33.0-fork.1"),
            listed("nightly"),
        ];
        assert_eq!(pick_release(releases).map(|r| r.tag_name), Some("v0.33.0-fork.1".to_string()));
    }

    #[test]
    fn the_fork_number_ranks_releases_of_one_version() {
        assert_eq!(version_rank("v0.33.1-fork.2"), (vec![0, 33, 1], 2));
        assert_eq!(version_rank("v0.33.1-fork.11"), (vec![0, 33, 1], 11));
        assert_eq!(version_rank("v0.33.1"), (vec![0, 33, 1], 0));
        assert_eq!(version_rank("0.33.1"), (vec![0, 33, 1], 0));
        assert_eq!(version_rank("v0.33.1+beta"), (vec![0, 33, 1], 0));
        assert_eq!(version_rank("nightly"), (vec![], 0));

        let releases = vec![listed("v0.33.1"), listed("v0.33.1-fork.1"), listed("v0.33.1-fork.11"), listed("v0.33.1-fork.2")];
        assert_eq!(pick_release(releases).map(|r| r.tag_name), Some("v0.33.1-fork.11".to_string()));

        // A plain upstream number still outranks a fork build of an older version.
        let mixed = vec![listed("v0.34.0"), listed("v0.33.1-fork.9")];
        assert_eq!(pick_release(mixed).map(|r| r.tag_name), Some("v0.34.0".to_string()));
    }

    #[test]
    fn the_notes_come_from_the_same_list_the_check_reads() {
        // GitHub answers /releases with each release's own body beside its tag, so the changelog pane
        // needs no second request and no repository this fork does not have. The quotes carry three
        // hashes because the sample body opens with a markdown heading, which ends a shorter raw string.
        let json = r###"{"tag_name":"v0.33.1-fork.1","html_url":"https://github.com/x/y/releases/tag/v0.33.1-fork.1","draft":false,"body":"## v0.33.1-fork.1\n- 38 new hooks"}"###;
        let release: Release = serde_json::from_str(json).unwrap();
        assert_eq!(release.tag_name, "v0.33.1-fork.1");
        assert!(release.body.starts_with("## v0.33.1-fork.1"));

        // A release published without notes reads as empty, which is what makes the dialog say so.
        let bare: Release = serde_json::from_str(r###"{"tag_name":"v0.33.1-fork.2","html_url":"","draft":false}"###).unwrap();
        assert!(bare.body.is_empty());
    }

    #[test]
    fn a_draft_and_an_empty_list_answer_nothing() {
        let drafts = vec![Release { tag_name: "v0.99.0".to_string(), html_url: String::new(), draft: true, body: String::new() }];
        assert_eq!(pick_release(drafts).map(|r| r.tag_name), None);
        assert_eq!(pick_release(vec![]).map(|r| r.tag_name), None);
    }
}
