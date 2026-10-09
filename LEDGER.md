# Defect ledger

Ledger for known defects in this fork. Not a changelog. This file is named LEDGER.md, renamed from
DEFECTS.md on 2026-10-08 so the name matches how it is used. Entries that quote the old name inside a
command or a citation are history and are left as they happened. Status marks:

- `[x]` fixed and verified in source
- `[~]` partially fixed, remainder listed
- `[ ]` open
- `[latent]` code path is defective but inert on the current client

Baseline build for this ledger: `e984b93`, deployed as `cri_mana_vpx.dll`, 40,142,336 bytes,
SHA256 `5C58010A45703B0DE38AD43797BEA090B4B9552FDFA66F3F04527FC2410D9D8C`.
Client: Umamusume Pretty Derby, Global region, Steam, Unity 2022.3.62f2 IL2CPP.

## A. Runtime findings, 2026-10-08 run

Evidence: `hachimi.log` 114.3 KB / 1136 lines, session 03:10:10 to 03:16:04 local,
and `hachimi/introspect.log` 1896.1 KB dumped by the same build.

- [x] **A1 `NowLoading::PlayFadeNowLoading` scales the wrong arguments.** 31 calls were logged
  as `NowLoading::PlayFadeNowLoading(0, 1, 0.3)` x15 and `(1, 0, 0.3)` x16. Arguments 0 and 1
  are the fade endpoints (alpha to and alpha from), argument 2 is the duration. The hook in
  `src/il2cpp/hook/umamusume/NowLoading.rs` scaled every float it receives, so at factor 20 the
  target alpha became 0.05 instead of 1. Fixed in `db3c272`, index 2 only is now scaled.
- [~] **A2 Overload matching is too coarse.** `symbols::get_method_overload` compares
  `Il2CppTypeEnum` only, and `CLASS` matches every reference type. The dump shows
  `SingleModeResultContentBase::FadeInContent/4` is not a duplicate pair but distinct overloads
  taking `UnityEngine.UI.MaskableGraphic` versus `UnityEngine.CanvasGroup`, and
  `FadeInContentFromBottom/4` takes `CanvasGroup` versus `Gallop.TextCommon`. Partly worked
  around: `SingleModeResultContentBase.rs` resolves its three fades through
  `AnimationSpeed::resolve_method` with an explicit parameter type list, and those hooks fired 16
  times in the 07:53 run. The coarse matcher itself is still in place for every other caller.
- [~] **A3 Static helpers are hooked for measurement, not for scaling.** The new static guard
  rejected two targets:
  `StoryTimelineController::GetTimeScaleByHighSpeedType/1 -> static float(bool)` and
  `GetTimeScaleHighSpeed/1 -> static float(bool)`. The guard prevented a wrapper that reserves a
  register for `this` from reading the `bool` from the wrong register. `StoryViewController::GetTimeScaleByHighSpeedType/0 -> static float()` confirms the existing
  no-`this` wrapper in `StoryViewController.rs` is correct. Done in `781e3e1`: both resolve through
  `resolve_static_method` behind argument only wrappers and log the value the game returns for each
  flag, six calls then one line every 4096. What is still open is whether the game calls them at all
  and what they return, which the next run has to answer before a factor is put on them. They remain
  the cheapest remaining story scale lever because both return the scale the story timeline then
  multiplies `deltaTime` by.
  C39 is the caution attached to that lever: the rule C35 put on the three installed story getters left
  a game value of 1.0 exactly as it was, so a factor on a getter that answers 1.0 changed nothing. A
  getter now scales on the read half, which raises 1.0 and protects only a value under it.
- [ ] **A4 Only 2 of 13 installed scaling points were reached.** Fired:
  `CountupModifier_getDuration 0.16 -> 0.008` and
  `StoryTimeline_getTimeScaleAfterEndStory 1 -> 5`. Installed with no call in this session:
  `TextModifier_getDuration`, `TextModifier_getDelay`, `TrainingFooter_GetCloseAnimWaitTime`,
  `TrainingFooter_GetItemAnimDuration`, `TrainingCuttClip_getDelayTime`,
  `SingleModeUtils_GetHighSpeedPlayDuration`, `SingleModeUtils_GetCutTimeScale`,
  `StoryTimeline_getTimeScaleEventWipe`, `TeamStadiumGrandResult_FadeInContentFromRight`,
  `FadeInContent`, `FadeInContentFromRight`, `FadeInContentFromBottom`,
  `SetHighSpeedFrameCount/1 (int)`, `ActivateSkipButton`, `PlayInNowLoading`, `PlayOutNowLoading`.
  Run 2 retires part of this list: `FadeInContentFromRight` reached the game 16 times.
  `ActivateSkipButton` now logs every call (`b138a9b`), so a run can tell an installed hook from one
  the game actually used.
  C39: both `StoryTimeline_getTimeScaleAfterEndStory 1 -> 5` lines above stopped reproducing on the
  build C35 produced. Its rule left a value at or below 1.0 exactly as it was, the hook became `1 -> 1`,
  and `AnimationSpeed::hit` writes no line when the value did not change, so the log said nothing at
  all. With C13 (0 of 61 duration fields resolve) and the two Story duration getters in the list above
  (installed, never called), nothing in `Group::Story` had a measured effect on that build while
  `story_speed` was still a slider on the Performance tab and a key in all ten locales.
- [ ] **A5 `PlayFadeFrontCanvas` is now hookable.** The dump resolves the blocked value type:
  `Gallop.NowLoading::PlayFadeFrontCanvas/5 -> void(float, float, float, class<System.Action>, struct<DG.Tweening.Ease:4B>)`.
  A 4 byte enum travels in a general purpose register, not XMM, so it can be passed through.
  Same applies to `StoryViewController::GetTimeScaleByHighSpeedType/1 -> static float(struct<Gallop.StoryTimelineController.HighSpeedType:4B>)`.
- [ ] **A6 `Show` / `Hide` still do not resolve.** `show_addr is null` and `hide_addr is null`
  persist, so `hide_now_loading` is inert. `NowLoading.rs:103-104` searches arity 8 and 4, which
  is the Japanese shape. An earlier dump reported `Show/7` and `Hide/3` on this client; this run's
  dump lists only `PlayFadeNowLoading`, `PlayFadeFrontCanvas`, `SetupCrossFadeAsync` and
  `PlayCrossFadeAsync` for the class, so the arity needs a targeted dump before the wrappers are
  rewritten. See A8.
- [~] **A7 `byref` parameters are ignored when matching, but are now verified before use.**
  `symbols::get_method_overload` still compares `Il2CppTypeEnum` only, and this client keeps the
  element type in the enum: the dumped `float&` and `int&` report R4 and I4, with the reference held
  in a separate bit of `Il2CppType`. `AnimationSpeed::resolve_ref_method` now checks that bit on
  every parameter of the resolved overload before a wrapper may write through it, and
  `StoryTimelineController::GetNextFrameCount_HighSpeed` is hooked through it (`be41b21`). A
  reference parameter and a value parameter of the same type are still indistinguishable to the
  matcher.
- [ ] **A8 The introspection dump is silently truncated.** `MAX_FULL_CLASSES = 500`
  (`src/il2cpp/introspect.rs:60`) was hit exactly, with no truncation marker, while the same run
  reported 10 name resolution failures. Absence of a name in `introspect.log` does not prove the
  name is absent from the client.
- [ ] **A9 The between-scene wipe is coroutine gated, and the load is the remaining cost.**
  `Gallop.NowLoading::SetupCrossFadeAsync/1 -> class<System.Collections.IEnumerator>(class<System.Action>)`,
  `PlayCrossFadeAsync/1 -> class<System.Collections.IEnumerator>(class<System.Action>)` and field
  `_isCrossFade [bool]`. The tween is DOTween based and the wait is a coroutine, so `ui_animation_scale`
  and `time_scale` already act on both. Measured residual per transition is 0.33 s to 3.2 s of
  async load, which no duration hook can shorten.
- [x] **A9 The static guard and the first hit logger both worked as designed** (2 rejections,
  2 hits logged).

### Measured baseline, transition cost

15 complete fade pairs (fade to black then fade from black) across 260.2 s of play:
gaps in ms 1139, 978, 650, 730, 2901, 326, 794, 2647, 337, 2164, 605, 1079, 3177, 392, 1024.
min 326 ms, median 1024 ms, max 3177 ms, total 18.9 s. `ui_animation_scale` was 1.0 in this run, so
the fades were not pre compressed. Any future claim of speedup must be measured against these
numbers, not against feel.

### Run 2, build `3adb2bf-dirty`, 07:53:03 to 08:00:52, 469.5 s

`hachimi.log` 277 KB / 2609 lines. 198 hook installs, 10 `_addr is null`, 22 name resolution
warnings, 2 static rejections, 0 panics. Deployed build after the fixes below: `db3c272`,
40,155,648 bytes, SHA256 `191F9B24A2666F105578FC9DD8A31D924E786910CB9E7A3772FE1B48A12E7E1B`.

- 47 wipe calls formed 23 in transition pairs: gaps in ms 980, 1022, 697, 19, 987, 676, 303, 599,
  2072, 377, 1177, 1146, 1185, 3657, 222, 969, 2137, 215, 2095, 604, 412, 1343, 959. min 19 ms,
  median 969 ms, mean 1037 ms, max 3657 ms, total 23.9 s, which is 5.1% of the session. The 19 ms
  pair shows the scaled fades themselves cost nothing once the target assets are already loaded.
- 22 screen intervals (fade from black to the next fade to black): min 1.5 s, median 9.5 s,
  mean 19.5 s, max 83.7 s, total 428.6 s, which is 91% of the session. That time is spent on
  screens rather than in animation, so it is where any further gain has to come from.
- `SingleModeResultContentBase::FadeInContentFromRight` fired 16 times carrying the shipped
  durations 0.03, 0.06, 0.09 and 0.12 s. `CountupModifier_getDuration 0.16 -> 0.008` and
  `StoryTimeline_getTimeScaleAfterEndStory 1 -> 5` fired again. `PlayInNowLoading`,
  `PlayOutNowLoading`, `SetupLoadingTips`, `getTimeScaleEventWipe`, `SetHighSpeedFrameCount` and
  `ActivateSkipButton` still appear only as install lines, never as calls.

### Run 3, career run, build `6333285` (merged upstream v0.32.0), 08:29:44 to 08:36:15, 391.5 s

`hachimi.log` 105 KB / 909 lines. 200 hook installs, 190 armed in one pass in 0.067 s, 10 `_addr is
null`, 5 class not found, 23 `= NULL` resolutions, 2 static rejections, 0 panics, 0 arming failures.
`time_scale` was 2 in this run, and the config snapshot recorded it.

- 22 wipe calls formed 11 in transition pairs: gaps in ms 1056, 810, 1149, 937, 2374, 181, 966,
  1012, 246, 1009, 927. min 181 ms, median 966 ms, max 2374 ms, total 10.7 s, which is 2.7% of the
  session. Every call carried 0.3 s raw and every call had its counterpart.
- 10 screen intervals: min 1.4 s, median 27.0 s, mean 37.4 s, max 115.1 s, total 373.7 s, which is
  95% of the session. Three runs now agree that the wipe path is nearly exhausted and what is left
  is time spent on screens.
- `ActivateSkipButton (auto_skip_result_screens true)` fired 5 times with zero
  `SkipFadeInTween unavailable`, which was read as the automatic result screen skip being confirmed
  end to end. That inference is not sound (C38): the exit that drops a skip request logged nothing,
  so 5 entry lines and 0 warnings describe 5 finished chains exactly as well as they describe a
  swallowed request. Run 7 can tell them apart. 15
  `FadeInContentFromRight` calls carried 0.03, 0.033, 0.06, 0.066, 0.09, 0.099, 0.12 and 0.15 s raw.
- `HighSpeedSetting` logged 73 read snapshots and exactly one write, `story high speed 0 -> 2 ...
  saved now 2`, plus `training high speed 0 -> 2, read back 2`. A10 stays closed.
- `GetNextFrameCount_HighSpeed` and `SetHighSpeedFrameCount` were installed and never called, while
  `StoryTimeline_getTimeScaleAfterEndStory 1 -> 5` and `CountupModifier_getDuration 0.6 -> 0.03`
  prove story and result code did run. See item 13.

### Run 4, partial career, build `6fdaac8` (with the observe only probes), 09:30:18 to 09:35:33, 315.1 s

`hachimi.log` 133 KB / 1026 lines. 215 hook installs, 205 armed in one pass in 0.068 s, 10 `_addr is
null`, 5 class not found, 2 static rejections, 0 panics, 0 arming failures, 0 `Attempted to get
invalid hook`. `Frame probe: 15 of 15 observe only probes installed` with no unresolved candidate,
which also confirms the hardened matcher rejected nothing that used to install: 0 `no matching
overload`, 0 `is not static`, 0 `passed by reference`.

- 8 wipe calls formed 4 in transition pairs: 1140, 598, 1091, 995 ms, total 3.8 s, which is 1.2% of
  the session. 3 screen intervals: 73.3 s to 122.7 s, median 95.6 s, total 291.7 s, 92.6%.
- `ActivateSkipButton` fired 0 times because this run never reached a result screen. `HighSpeedSetting`
  logged 40 snapshots and 1 write, `story high speed 0 -> 2 ... saved now 2`, plus
  `training high speed 0 -> 2, read back 2`.
- Probe totals at the end of the session: `IsHighSpeedMode 34635`, `IsStoryEndFrameOrGrandLiveWaitFrameSkipped 8043`,
  `SkipMotionFrame 714`, `SkipFrameCount 579`, `get_WaitFrameCountUntilNextBlock 404`,
  `UpdateTimeScaleByHispeedType 211`, and zero for `SetFrameCountForWaiting`, `get_WaitFrameUntilNextBlock`,
  `set_WaitFrameCountUntilNextBlock`, `set_WaitFrameUntilNextBlock`, `get_WaitingFrameCount`,
  `get_TimeScale`, `set_TimeScale`, `IsSkipToTextClip` and `GetWaitFrameUntilNextBlockLocalize`.
- `SkipFrameCount` carried 76, 182, 313, 448, 583 and 125 frames with both flags 0, and `SkipMotionFrame`
  received the same frame counts, so the two move together. `IsHighSpeedMode` returned 0 in every
  logged call. See item 14.

### Run 5, career story with the skip extension on, build `6826c8b`, from 12:15:44, story stalled

`hachimi.log` 107 KB / 865 lines. 187 hooks armed in one pass in 0.053 s, `13 of 13 observe only
probes installed`, 10 `_addr is null`, 5 class not found, 2 static rejections, 0 panics. The snapshot
carried `story_high_speed true`, and the config file still holds `story_skip_frame_scale 1.5`.

- The story froze. `get_WaitFrameCountUntilNextBlock` climbed 4, 12, 24, 36, 44, 48, 84 and then
  stopped at 96 from 190 s onward. `UpdateTimeScaleByHispeedType` stopped at 49. `IsHighSpeedMode`
  ran at a flat 3600 calls per 20 s interval, about 180 a second, with nothing advancing.
- Every skip step hit the ceiling the option imposed: `SkipFrameCount 133 -> 733`, and the game then
  handed that same 733 to `SkipMotionFrame`, which this fork raised again to 1333. All twelve logged
  steps were exactly +600 frames, so the multiplier in effect was at least 5.5 while the recorded
  setting was 1.5. The magnitude is not explained by the snapshot, and the option has been removed
  rather than debugged. See C34.
- The high speed mode engagement wrote nothing: no `asked the story timeline` line and no rejection
  line, while the probe still reported `IsHighSpeedMode()` returning 0 in its six samples. It had
  three exit paths that logged nothing at all, which is what made this run unreadable. Every exit now
  logs a reason, and the config mirror logs the one line that proves the option reached that layer.
- Recovery on the user side was to lower the value and change story speed, which re-ran the apply
  path.

### Run 6, career story with the skip extension removed, build `3565b49`, 12:32:51 to 12:35:19, 148 s

`hachimi.log` 109 KB / 908 lines. 187 hooks armed in one pass in 0.052 s, `13 of 13 observe only
probes installed`, 10 `_addr is null`, 5 class not found, 2 static rejections (A3), 0 panics. The
snapshot carried `story_high_speed true` and no skip scale at all.

- Story advanced normally. `get_WaitFrameCountUntilNextBlock` read 4, 12, 44, 72, 84, 116, 148
  across the 20 s reports and `UpdateTimeScaleByHispeedType` read 0, 39, 77, 81, 93, 109. No
  plateau and no spin, so removing the extension cleared the run 5 stall.
- The skip paths read as the game's own values and carry the same number on both sides: 124, 189,
  289, 502, 597, 693 in six pairs inside 30 ms. They fire in a burst when the timeline jumps
  blocks, not on a steady cadence, which is what made the 10 s activity window miss.
- The game's story high speed mode was off for the whole session: `IsHighSpeedMode` polled 19737
  times, about 133 a second, and returned 0 in all six samples; `IsStoryEndFrameOrGrandLiveWaitFrameSkipped`
  returned false in all six of its samples across 6086 calls; `get_WaitFrameCountUntilNextBlock`
  returned 1 every time it was sampled.
- The engagement still wrote nothing, and its six diagnostic lines were all spent at 12:32:52, six
  seconds after install in the menu phase, so the attempt that mattered printed nothing and the run
  cannot say which exit it took. Fixed with one log slot per reason plus an attempt line that
  carries the max value, the mode read and the predicate result, and the activity window widened
  from 10 s to 300 s.
- This client's dump (27 657 lines) carries the game's whole auto high speed subsystem, which is a
  better door into the mode than writing the timeline static by hand: `StoryManager::
  ChangeAutoHighSpeedSettingAndSave/1`, `ChangeSavedAutoHighSpeedSetting/0`,
  `ResetSavedAutoHighSpeedSetting/0`, `StoryManager::get_IsHighSpeedMode/0 -> static bool()`,
  `StoryViewController::LoadAutoHighSpeedSetting/0`, `SwitchAutoHighSpeedSetting/0`,
  `UpdateAutoHighSpeedSettings/1 -> void(bool)`, `ApplyAutoHighSpeedSettings/0`,
  `PauseAutoHighSpeedSettings/0`, `ResumeAutoHighSpeedSettings/0`, and `StorySceneController::
  ApplyAutoHighSpeedSettingsToAllCharacters/1 -> void(bool)` plus
  `ApplyAutoHighSpeedSettingsToModel/2 -> static void(class<EventTimelineModelController>, bool)`.
- The two A3 rejections are these `StoryTimelineController` statics, and the dump confirms their
  shape: `GetTimeScaleByHighSpeedType/1 -> static float(bool)` and
  `GetTimeScaleHighSpeed/1 -> static float(bool)`, a bool rather than the enum, which is why the
  instance style wrapper kept being refused. Around them:
  `field <TimeScale>k__BackingField [static float] = 0`, `get_TimeScale/0 -> static float()`,
  `set_TimeScale/1 -> static void(float)`, `get_TimeScaleEventWipe`, `get_TimeScaleAfterEndStory`,
  `UpdateTimeScaleByHispeedType/0 -> void()`. High speed mode is a first class feature with its own
  fade and audio handling: `field FADE_TIME_FOR_HIGH_SPEED [public static const float]`,
  `SetBgmClipEnabledOnSwitchHighSpeedMode/0`, `ForceStopBgmOnHighSpeed/0`.
- Waiting and skip paths the ledger has not used yet, recorded here so the next search starts from
  the dump rather than from guesses: `GotoBlockForSkip/2 -> void(int, bool)`,
  `IsSkipOrBlockByWipe/1 -> bool(class<StoryTimelineBlockData>)`,
  `NeedsCheckForWaiting/1 -> bool(int)`, `StopWaiting/0`, `StartWaitingForUserInput/1`,
  `WaitInput/3`, `CheckGotoNextAndWaitInput/4 -> void(bool&, bool&, class<StoryTimelineTextClipData&>&, bool&)`,
  `field _frameCountForWaiting [int]`, `field <IsWaiting>k__BackingField [bool]`.

- [x] **A10 `HighSpeedSetting` re-applied the same write at every scene change.** 50 lines read
  `story high speed 1 -> 2 via StoryManager::SaveHighSpeedType, read back 1`. `SaveHighSpeedType`
  moves StoryManager's own saved setting, the `saved story` value did go 1 to 2, but
  `ApplicationSettingSaveLoader::get_StoryHighSpeedType` kept reporting 1 and the idempotency
  check used the loader value. Fixed in `db3c272` by comparing against `GetSavedHighSpeedSetting`.
- [ ] **A11 `GetMaxHighSpeedType` is context dependent.** Two snapshots in one session:
  `max 1, saved story 1, loader story 1, training 2` x27 at menus, and
  `max 2, saved story 2, loader story 1, training 2` x50 once story content was reached. The game
  ceiling is 2 in story context, the story setting was genuinely raised from 1 to 2, and training
  already sat at 2. A ceiling read in the wrong context raises nothing.
- [x] **A12 Hook installation costs about 4.6 s of every launch.** `Initializing il2cpp hooks` at
  07:53:03.332 to `Hooking finished` at 07:53:08.013, with `MH_EnableHook: MH_OK` events spaced at a
  uniform 24 ms. 192 enables at that spacing is 4.6 s. MinHook separates building a detour from
  arming it, so `Interceptor::begin_batch` and `finish_batch` create every hook during `hook::init`
  and arm them in one `MH_EnableHook(MH_ALL_HOOKS)` pass (`865935f`). A failed batch arms each
  target on its own rather than leaving the build unarmed. Android keeps arming at create time, so
  the batch is a no-op there. The line to read in the next run is
  `Hooking finished: N hooks armed in one pass, S s`, and S is what has to beat 4.68.
- [~] **A13 The scaling direction of `GetNextFrameCount_HighSpeed` is not known yet.** The
  signature `void(float&, int&)` does not say which half is the wait between story characters and
  which half is the step the timeline advances. The pair comes out of the game's readonly
  `_highSpeedFrameCountArray`, so the int is a candidate index into that array rather than a wait
  length, and dividing an index shortens nothing: `AnimationSpeed::scale_frame_count` floors a
  positive result at 1, so a divided index selects a different entry, and that entry can hold a
  longer frame count than the one the game chose. Both scalings on this path are now removed.
  `GetNextFrameCount_HighSpeed` reads the pair out of the caller's storage, logs it for its first
  six calls behind a plain counter, and writes nothing back; `SetHighSpeedFrameCount` logs the value
  it receives and hands it on untouched - the decision already taken for the two skip hooks after
  run 5 (C34). Runs 3 and 4 produced no scaling line for either path, and run 5's twelve `Story step`
  lines were all skip paths, so no run has read a single value off this pair yet. Item 16 keeps that
  gap on the record.
- [x] **A14 A run did not record the settings it ran with.** `hook::init` now writes one
  `Config snapshot:` line carrying transition, result, story, ui_animation, time_scale, story_tcps,
  choice_delay, target_fps, auto_skip_result, high_speed_settings, hide_now_loading and the physics
  mode (`865935f`), which is what made run 2 ambiguous: `config.json` read 3.0 and 1.0 at 03:31 and
  1000.0 and 1000.0 at 04:00 with nothing in the log to say which values were live.

- [x] **A17 The two story high speed time scale helpers have been read in game.** Run 7 logged six
  `Story scale StoryTimelineController::GetTimeScaleByHighSpeedType(0) call N -> 8` lines: the game asks
  the helper with the flag false and it hands back 8.0, and `GetTimeScaleHighSpeed` was not called once in
  534 s. That is the answer A3 installed the pair to get (`781e3e1`, run 7). It also bounds the next
  decision: 8.0 sits above `MAX_TIME_SCALE = 5.0`, so a factor on that return passes the ceiling rule
  untouched, and any lever placed there needs a ceiling decision first.

### Run 7, first run on the delta fix build `51941ea-dirty` (deployed `2E36C270`), 17:12:47 to 17:21:42, 534 s read, session still open

`hachimi.log` 157 KB / 1342 lines. 189 hooks armed in one pass in 0.075 s, 10 `_addr is null`, 5 class
not found, 0 static rejections, 0 panics. The snapshot reads `transition 20 result 20 story 10
ui_animation 1000 time_scale 2 story_tcps 1000 choice_delay 0.0001 target_fps 60
target_fps_unfocused -1 auto_skip_result true high_speed_settings true story_high_speed true
hide_now_loading false physics Some(Mode60FPS) cyspring_mono_uncap_frame_scale true`, so the two names
C30 asked for are now printed. Note that `ui_animation 1000` and `choice_delay 0.0001` are the values in
`config.json`; the code now clamps them to 20 and 0.1 (C5, C24).

- The value shape proofs landed where C25 predicted: five lines, every one
  `Gallop.StoryTimelineController.HighSpeedType:4B`, plus `HighSpeedSetting: StoryManager statics max
  struct, saved struct, setter struct` and `story high speed helpers setter true (struct), value predicate
  true (struct), state reader true, baseline recorder 1`. The two `is static` rejections run 6 produced are
  gone. One refusal appeared instead: `StoryTimelineTrainingCuttClipData.DelayFrame is not static`, a name
  the matcher declined to reach through a `this` wrapper. It is a lead for the story stepping search, not a
  regression: nothing used it before either.
- The engagement wrote four times in 534 s and each write is accounted for: `asked the story timeline for
  high speed type 2 (write 1, story state 8)`, `(write 2, story state 10)`, `(write 3, story state 24)`,
  `(write 4, story state 28)`, every one `IsHighSpeedMode 0 -> 1`. The `story state` number is the game's
  own write count on the static, so the applied marker held to one write per state change. One exit logged
  `story high speed attempt, StoryManager max 2, IsHighSpeedMode 1, IsHighSpeedMode(2) 1, story state 1,
  written for state 18446744073709551615` and wrote nothing, because the game already had the mode on.
  Outside story it stayed quiet: `story high speed mode left alone, the story stepping paths have been
  quiet since the last scene` at 21:20:59.
- The bool gate reads correctly in game: `IsHighSpeedMode(2) 1`, and the probe's `story high speed mode 1
  writes 4` agrees with the four engage lines.
- The story side of the mode is a repeated handshake, not a one time switch. The game clears the mode
  between clips (state 1 to 8 to 10 to 24 to 28), so keeping it on costs four writes per story session and
  each write replays the game's own mode switch side effects. Item 22's argument for driving the mode
  through the game's auto high speed setting is now measured rather than assumed.
- `Time::set_timeScale` was called three times in 534 s, all `1 -> 1 (lever x2)`, while
  `UpdateTimeScaleByHispeedType` ran 46 times and the `get_TimeScale`/`set_TimeScale` property probes stayed
  at 0. Story high speed playback speed does not travel through `Time.timeScale` on this client, which is
  the practical content of C35's note that the lever acts on values above 1.0 only: `time_scale 2` changed
  nothing this run, by design rather than by failure.
- The story getter lever is live: `AnimationSpeed: StoryTimeline_getTimeScaleAfterEndStory 1 -> 5`, with
  `story 10` on the snapshot line and the product stopped at `MAX_TIME_SCALE`. `getTimeScaleEventWipe` and
  `SingleModeUtils_GetCutTimeScale` were installed and not reached.
- The frame chain carries plain numbers. `SkipFrameCount` and `SkipMotionFrame` logged identical values one
  to one, 120, 225, 325, 523, 524, 126 and 202, and story advanced normally. No stall, no ceiling
  clustering, nothing forwarded scaled, which is what C34 asked the next story run to show.
- The speed pass is quiet: `apply pass 1` to `6` read 4, 7, 10, 13, 16 and 19 config values with one entry
  lock each, 0 table passes, 0 field reads, 0 field writes, and no pass 64 line appeared, so fewer than 64
  passes ran in 534 s. Pass 1 is stamped 21:12:48.304, after `Hooking finished` at 21:12:48.009, so nothing
  applied inside the batch window (C37).
- Wipes: 7 pairs, 759, 823, 813, 888, 1039, 1080 and 3308 ms, total 8.7 s, 1.6% of the 534 s read here. The
  gaps *between* wipes were 15.6, 42.4 and 21.4 s, which is where the remaining wall clock sits (A9,
  section 8 item 5).
- `IsStoryEndFrameOrGrandLiveWaitFrameSkipped` polled 15180 times, samples `[3.0, 45.0, 0.0]` and
  `[1.0, 45.0, 0.0]`, false every time. About 28 polls a second averaged over the session, each one paying
  the `get_orig_fn!` map lookup C33 is about.
- `HighSpeedSetting` produced no write line at all this run: the saved story setting was already 2 from an
  earlier session, and every scene snapshot with `max 1` left `saved story 2` alone. That is the raise only
  rule behaving, but it means the write direction fix was not exercised by a fresh default.
- Not exercised: `auto_skip_result_screens` (no result screen in this session, no `SkipFadeInTween` line, no
  `skip guard` contention line) and the restore paths (both options stayed on, so no `restored to the` line).
- After 198 s no story content ran at all: every story counter is identical between the 198 s and 534 s
  totals lines, while the player was on screens.

Closed by this run, test plus run line in hand: A17, C36, C37, C39, and fix order items 20, 21, 26 and 28.
Item 27 is closed as a reading task, the line exists and reads 4, 7, 10, 13, 16 and 19 config reads with
one entry lock each and no field call, which is the shape a run was expected to produce. Item 24 stays
`[~]`: the recorder armed path is proven (four writes, one per state change, nothing written for a mode the
game already had on), but the case with no recorder armed never happened in this session. C41 stays `[~]`:
the apply half is a run's number now, while the `HighSpeedSetting` and `StoryTimelineController` write halves
are still test models. C38, C40, C42, C43 and item 29 were not exercised (no result screen, no downward scale
left to reach, no android device, and the two high speed frame count hooks were never called), and items 22,
23 and 25 stay open.

Ledger self check for this change set, measured against the diff it prints: 4 bracketed closures awarded
(`A17` created at `[x]`, `C36`, `C37`, `C39`), 5 numbered closures awarded (items 20, 21, 26, 27, 28), 0
taken away. Bracketed status totals after the edit: 11 `[x]`, 19 `[~]`, 36 `[ ]`, 3 `[latent]`.

### Run 8, completed career on the same build (`51941ea-dirty`), 17:33:08 to 17:39:10, 362 s

`hachimi.log` 141 KB / 1147 lines. 189 hooks armed in one pass in 0.059 s (528 log lines before that
line), 10 `_addr is null`, 5 class not found, 1 static refusal (`StoryTimelineTrainingCuttClipData.DelayFrame
is not static`, same as run 7), 0 panics. `introspect.log` written again, 1,941,603 bytes, because
`debug_mode` is on.

- New reading: the session split by `SceneManager: next_view_id` and the `ViewId` names in
  `src/il2cpp/hook/umamusume/SceneDefine.rs`. Attributed dwell, 353.3 s of 362 s: Title 127.8,
  SingleModeMain 104.7, SingleModePaddock 39.7, SingleModeConfirmComplete 13.4, Story 12.7,
  SingleModeResult 12.6, HomeHub 11.7, Home 8.2, Mission 7.9, Splash 6.6, GachaMain 6.0,
  SingleModeMonthStart 2.0. Launch to first training screen was 140 s, and the mod's own part of that is
  0.059 s of hook arming.
- Wipes: 15 pairs, 18 ms to 2242 ms, 15.4 s, 4.3% of the session. The transition lever is a rounding
  error next to the buckets above.
- Result screens were reached for the first time since the guard change: 5 `ActivateSkipButton
  (auto_skip_result_screens true)` lines from 17:38:22 to 17:38:29, 24 `FadeInContent*` lines with the
  durations scaled (`0`, `0.03`, `0.06`, `0.09`, `0.12`, `0.15`), no `skip guard busy` line and no
  `SkipFadeInTween unavailable` line, so nothing was dropped and nothing failed to resolve. C38's open
  question (one skip per result part) is still open: five entries and five parts matching is not proven by
  these counts.
- Story in this career was short: 8 `Story scale ... (0) -> 8` reads, three engage writes (states 36, 40,
  44), one `story high speed type 2 already written for story state` line, one
  `story high speed mode left alone, the game already ...` exit, and `SkipFrameCount`/`SkipMotionFrame`
  again identical one to one (117, 193, 194, 114, 223, 328, 437). `StoryTimeline_getTimeScaleAfterEndStory
  1 -> 5` printed once. `story high speed mode 0 writes 3` on the last totals line.
- `Time::set_timeScale` was called once, `1 -> 1 (lever x2)`.
- Three options that are configured and never reach the game in a whole career: `story_tcps_multiplier 1000`
  (only its `story_tcps` name on the snapshot line), `ui_animation_scale 1000` (one `TweenManager` install
  line, no call), `cyspring_mono_uncap_frame_scale true` (name on the snapshot line, no call). See A18.

### Run 9, training heavy career run on the release build `v0.32.0-d42b6d9`, 23:05:42 to 23:16:01, 617.5 s

Build: the release asset binary, 29,352,448 bytes, SHA256 `1E689C12106242ACFA02E981284375D4B71313CDF93AC76029DD34F5226D673B`,
reporting `v0.32.0-d42b6d9` (`hachimi.log:1`). The run started before `v0.32.0-974da9c` was deployed, so it carries
the probe but neither the screen attribution nor the scaling point counters. `debug_mode` on, `Hooking finished: 207
hooks armed in one pass, 0.065 s` (L568). Config snapshot L12 is identical to run 8, and `HighSpeedSetting` again
reports `training 2` (L596), the game's own maximum.

Session 617.5 s from the first view change to the last log line. Dwell by view, summing the gap to the next view
change: SingleModeMain 1101 523.4 s over 14 visits, SingleModePaddock 1200 48.6 s over 7, SingleModeMonthStart 1100
8.5 s over 6, Story 3000 7.8 s over 6, SingleModeSuccessionEvent 1501 7.6 s, SingleModeStart 1000 7.2 s, Splash 3.7 s,
Title 2.8 s, HomeHub 2.7 s, SingleModeSuccessionCut 1500 0.9 s. 85% of the session sits on 1101, which is where the
player decides and where the training animation plays, so a view change cannot separate thinking time from animation
time. That is exactly the gap the probe was meant to fill.

Probe install: `18 of 19 observe only probes installed` (L550). The one failure is
`TrainingParamChangeUI::InitializePlateList` (L551), and the dump now says why (C48).

Reached and counted, from the totals lines L801 to L999:

| probe | calls | peak value |
|---|---|---|
| `CutInTimelineController::UpdateSpeed` | 3194 | none printed, void |
| `SingleModeTrainingCutInHelper::GetTargetSpeed` | 824 | 8.334 |
| `SingleModeUtils::GetTrainingCutTimeScale` | 124 | 8.334, pairs `[3.04, 6.08]` and `[2.4, 4.8]` |
| `CutInTimelineController::SkipRuntime(time)` | 70 | `0.0` and `-1.0` |
| `SingleModeTrainingCutInHelper::IsHighSpeedMode` | 67 | bool, not printed |
| `SingleModeMainTrainingCuttController::WaitTapAsync` | 31 | |
| `SingleModeMainTrainingCuttController::FadeOutResultFlash` | 31 | |
| `SingleModeTrainingCutInHelper::SkipRuntime` | 16 | |
| `SingleModeMainViewTrainingCutStatus::Skip(skip)` | 16 | every call with the argument false |

Never reached in 617 s of training: `CutInTimelineController::ResetCurrentTime`, `SetSpeed`, `get_CurrentTime`,
`get_CurrentTimeScale`, `get_WaitingTime`, `SkipRuntimeFrames`, `SkipTimeDirect`, `SingleModeMainViewController::WaitTap`,
`CoroutineDoTweenTimeScale`. `cut runs 0` in all 22 totals lines, so the wall clock half of item 34 produced no number
at all. The boundary was chosen from the class name and this client does not drive a training cut-in through
`ResetCurrentTime` (C47).

What the counts do say: 31 `WaitTapAsync` with 31 `FadeOutResultFlash` is 31 training cut plays, each of which reached
the game's own tap wait, and `Skip` was asked 16 times and answered false every time. The cut already runs at up to
8.334 (`GetTrainingCutTimeScale` returns double what it is given, and `GetTimeScaleByHighSpeedType` reached 8 in the
story path at L784), which is the game's own high speed ceiling. The remaining lever on this path is the game's own
skip and autoplay doors (A28, A29), not another multiplier.

The five training scaling hooks installed in run 8 printed an install line (L458 to L478) and again no call line, which
is the fact `95bf6d1` exists to settle. Their counters appear on the probe totals line from `v0.32.0-974da9c` onward.

Story side of the same run: `GetTimeScaleByHighSpeedType(0)` returned 1 three times early and 8 from L784 on,
`IsHighSpeedMode 0 -> 1` was asked 7 times (L770, L904, L940, L959, L976, L982, L989),
`StoryTimeline_getTimeScaleAfterEndStory 1 -> 5` (L815), `CheckChoiceAutoTap 0.1333528 -> 1.000146` (L855), and
`Time::set_timeScale call n: 1 -> 1 (lever x2)` six times (L856 to L904). Story was 7.8 s of a 617.5 s session, so in
this session the training turns are the time, not the story.

## A18 and C44, added from run 8

- [ ] **A18 Three performance options are installed and never called on this client.** A whole career,
  including result screens and eight story clips, produced one install line and no call line for the DOTween
  `TweenManager` hooks (`ui_animation_scale`), none for the story typewriter path (`story_tcps_multiplier`),
  and none for `cyspring_mono_uncap_frame_scale`. All three are offered in the Performance tab with values
  1000, 1000 and true. That is the C14 condition: an option that does nothing here is being presented as if
  it works. Either the hook is on the wrong method for this client or the option belongs behind a region or
  feature gate; a decision, not a code guess.
- [ ] **C44 `debug_mode` writes a 1.9 MB `introspect.log` on every launch.** 1,941,603 bytes at 17:33:08 in
  run 8 and the same size in run 7, from inside the init window that C25 says sits under the loader lock.
  It is our cost, it is avoidable at launch, and it is the largest single thing the mod writes per start.
  Not measured as a duration yet: the log has no timestamp pair around the dump.

## A19 to A25, the training screen research (2026-10-08)

Sources: a token index built from `UmamusumePrettyDerby_Data\il2cpp_data\Metadata\global-metadata.dat`
(281,297 unique identifiers, written to `hachimi-global-recon\pc_metadata_tokens.txt`) and the mod's own
`introspect.log` dump. Line numbers below are in `hachimi\introspect.log`.

- [ ] **A19 The forced training animation is the game's own cut-in ("Cutt") subsystem, and it is a
  timeline of motions, not a duration field.** The names are `TrainingCuttController` with scenario
  variants `SingleModeMainTrainingCuttController`, `TrainingCuttLiveController`,
  `TrainingCuttVenusController`, `TrainingCuttTeamRaceController`, `TimelineTrainingCuttController`, and for
  the friendship case `TagTraining`, `TagTrainingCutInPlayer`, `SingleModeMainViewTagTrainingCutInPlayer`,
  `LiveBonusTagTraining`, `IsTagTraining`, `IsTagTrainingGroupSupportCard`, `SetupTagTrainingEffect`. The
  animation is assembled from paths: `TrainingCutInCuttPath`, `TrainingCutInBodyMotionPath`,
  `TrainingCutInCameraMotionPath`, `TrainingCutInFacialMotionPath`, `TrainingCutInPositionMotionPath`,
  `TrainingCutInEarMotionPath` (dump L22849-22854) and `GetTrainingCutInCuttPath/6`,
  `GetTrainingCutInMotionSuffix/2`, `GetTrainingCutInBgPrefabPath/4` from the resource path class (dump
  L20323-20331). Prefab names in the string literals: `pf_fl_singlemode_tagtraining_cutin00`,
  `pfb_uieff_single_tagtraining_line_effect_00`, `pfb_uieff_single_start_tagtraining_particle_00`.
- [ ] **A20 The game already has skip and speed doors for it, in its own code.** Signatures read off the
  dump, verbatim:
  `Gallop.SingleModeTrainingCutInHelper::SkipRuntime/0 -> void()` (L26839),
  `Gallop.SingleModeTrainingCutInHelper::GetTargetSpeed/0 -> float()` (L26840),
  `Gallop.SingleModeTrainingCutInHelper::IsHighSpeedMode/0 -> static bool()` (L26841),
  `Gallop.SingleModeTrainingCutHelperExtension.ContextExtension::SkipRuntimeAll/1 -> static void(IList<SingleModeTrainingCutInHelper>)`
  (L26933), `::FixedUpdateForHighSpeed/1` and `/2 -> static void(IList<...>, float)` (L26934-26935),
  `::SetTimeAll/2 -> static void(IList<...>, float)` (L26936), `::SkipPause/1` (L26937),
  `::GetCurrentTime/1 -> static float(IEnumerable<...>)` (L26938),
  `Gallop.SingleModeUtils::GetTrainingCutTimeScale/1 -> static float(float)` and
  `Gallop.SingleModeUtils::GetCutTimeScale/0 -> static float()` and
  `Gallop.SingleModeUtils::GetHighSpeedPlayDuration/0 -> static float()` (L25950-25952),
  `Gallop.CutIn.CutInBgModel::set_PlaySpeed/1 -> void(float)` and `::SetTime/1 -> void(float)`
  (L26939-26941), `Gallop.TimelineTrainingCuttController::field DelayTime [public float]` (L26924),
  `Gallop.StoryTimelineTrainingCuttClipData::field DelayFrame [public int]` (L26903). The cut-in helper
  family uses the same shape elsewhere: `Gallop.TeamBuildingEndingCutInHelper::SkipFrame/0 -> void()` and
  `::IsRunningSkip/0 -> bool()` (L26835-26836), and the cut-in event params include
  `CuttEventParam_JumpFrame`, `CuttEventParam_SetTapJumpFrame`, `CuttEventParam_OnTapNextFrame`.
  `Gallop.SingleModeMainTrainingCuttController::field _isPlayedFixedUpdateForHighSpeed [bool]` (L25508) and
  `::WaitTapAsync/0 -> IEnumerator` (L25500) say the cut-in has a high speed path already and that part of
  it waits for a tap.
- [ ] **A21 The mod currently changes nothing on the training screen.** Four of the field specs in
  `AnimationSpeed.rs:80-84` (`TrainingParamChangeA2U.ANIMATION_TIME_HIGH_SPEED`,
  `TrainingParamChangePlate.TYPEWRITE_DURATION`, `TrainingParamChangePlate.NEXT_WAIT_DURATION`,
  `SingleModeMainTrainingCuttController.FLASH_LABEL_SPEED_UP_SUCCESS_IN` and
  `FLASH_LABEL_SPEED_UP_FAILURE_IN`) are refused as compile-time constants, and the one story-side attempt
  `StoryTimelineTrainingCuttClipData.DelayFrame` is refused as "is not static" (run 7 and run 8 log lines)
  because the dump says it is `public int`, an instance field. The five installed hooks
  `TrainingFooter_GetCloseAnimWaitTime`, `TrainingFooter_GetItemAnimDuration`, `TrainingCuttClip_getDelayTime`,
  `SingleModeUtils_GetHighSpeedPlayDuration` and `SingleModeUtils_GetCutTimeScale` printed
  `new_hook!` lines in run 8 (`hachimi.log:464,466,468,474,478`) and no call line for the whole career,
     while `CountupModifier_getDuration` did print one, so the call logging itself works. The two footer
   getters sit on `Gallop.SingleModeMainViewTrainingFooter`, where the dump marks
   `GetCloseAnimWaitTime/0 -> static float()` as static and `GetItemAnimDuration/1 ->
   float(class<Gallop.SingleModeMainViewTrainingFooterItem>)` as an instance method (L25725-25726);
   `resolve_getter` documents that a zero argument static is harmless here (`AnimationSpeed.rs:781-784`).
   `StoryTimelineTrainingCuttClipData.DelayFrame` is an instance field, so it needs a method door and not
   a static write.
  - The evidence that a hook was reached is weaker than it looks. `hit()` (`AnimationSpeed.rs:811`) logs
    only when `raw != scaled`, so a call that returns `0.0` is scaled to `0.0` and prints nothing. "No
    call line" means "never changed anything the mod could see", not "never called", so a probe for this
    work has to count calls rather than changed values (item 38).
- [ ] **A22 The cut-in has a playback engine with its own skip doors and its own speed doors.**
  `Gallop.CutIn.Cutt.CutInTimelineController` carries both families, verbatim from the dump:
  `set_SkipFrame/1 -> void(int)` (L26955), `SkipRuntime/2 -> void(int, bool)` and `SkipRuntime/1 ->
  void(float)` (L26967-26968), `SkipTimeDirect/1 -> void(float)` (L26969), `SkipAutoUpdate/1` in two
  overloads, one taking `struct<...SkipType:4B>` and one taking a `List<TimelineEffectList>`
  (L26976-26977), plus the waiting gates `get_WaitingTime/0`, `get_IsWaitingUpdate/0`,
  `SetEnableWaitingUpdate/1`, `AddWaitingTime/1`, `GetNextWaitTapTime/0` and `UpdateNextWaitTapTime/0`
  (L26952-26966, L26979-26980). Its rate side is `get_Speed/0`, `SetSpeed/1`, `GetCurrentSpeed/0`,
  `UpdateSpeed/0`, `UpdateCharacterSpeed/2`, `UpdateEffectSpeed/2`, `SetCurrentTime/1`,
  `ResetCurrentTime/0`, `get_CurrentTimeScale/0`, `get_CurrentCySpringTimeScale/0` and
  `AlterUpdate_TimeScale/0` (L26958-26985) over the instance fields `_speed` (L27009), `_prevSpeed`
  (L27007), `_timeScaleFromCurve` (L27011), `_cySpringTimeScaleFromCurve` (L27012) and
  `_tempTimelineKeyTimeScaleData [class<Gallop.CutIn.Cutt.TimelineKeyTimeScaleData>]` (L27013). Speed
  inside the cut asset is keyed per clip: `TimelineKeyCharacterMotionData::get_{Body,Facial,Ear,
  Position}ClipPlaySpeed/0 -> class<Gallop.CutIn.Cutt.KeyFloat>` (L27056-27059) returns a keyframe
  container and not a float, so it is unreachable through a getter hook.
- [ ] **A23 The training screen has its own skip UI, typed by the game's own high speed enum.**
  `Gallop.PartsSingleModeCommonFooter::UpdateSkipButton/0`, `::OnClickSkip/0` and `::SetSkipButton/1 ->
  void(struct<Gallop.StoryTimelineController.HighSpeedType:4B>)` (L25706-25708), plus
  `Gallop.SingleModeMainViewTrainingCutStatus::Skip/1 -> void(bool)` (L25719) and
  `Gallop.SingleModeMainViewTrainingCutStatusFrame::Skip/5 -> void(int, int, int, int, int)` (L25721).
  `SingleModeMainViewTrainingCutStatus::WillRankUpInHighSpeedMode/0 -> bool()` (L25718) shows a high
  speed branch in the training status animation. `Gallop.StoryViewController::SkipTrainingCutt/0 ->
  void()` (L26187) belongs to the story timeline path and not to this screen, which matches
  `TrainingCuttClip_getDelayTime` being installed and never called.
- [ ] **A24 The game ships a 3X cut path and a short motion variant, both as literals.**
  `Gallop.SingleModeDefine::field CUT_PLAY_SPEED_1X`, `CUT_PLAY_SPEED_3X` and `CUT_PLAY_DURATION_3X`, all
  `public static const float` (L25687-25689), and `Gallop.TrainingParamChangeUI::field PLAY_SPEED_DEFAULT`
  and `PLAY_SPEED_FOR_SKIP` (L26128-26129) prove a fast form exists whose numbers are literals, so only
  its readers can be hooked. `Gallop.TrainingParamChangeA2U::GetSkipMotionName/1 ->
  string<System.String>(int)` (L26107) picks a skip variant by index, which is cheaper than speeding the
  long motion. `Gallop.TrainingParamChangeUI` holds the post-training plate cascade as instance fields
  `_delay`, `_tapWait`, `_groupInterval`, `_sequenceInterval` and `_forceTapWait` (L26130-26134) reached
  through `InitializePlateList/2 -> void(List<...ChangeParameterInfo>, float)` (L26121).
- [ ] **A25 The cut-in coroutine snapshots the clock at its start.** `<PlayTrainingCut>d__70::field
  <timeScale>5__7 [float]`, `<isHighSpeedOnStart>5__9 [bool]`, `<waitForFixedUpdate>5__10
  [class<UnityEngine.WaitForFixedUpdate>]` and `<allTextWaitTime>5__12 [float]` (L25496-25499) name one
  entry point spelled `PlayTrainingCut` with a single t, distinct from the `TrainingCutt` names, that reads
  a time scale and the high speed state when it starts, waits one fixed update, and holds its own text
  wait. Its compiler index 70 sits below `WaitTapAsync`'s lambda at 81 and `FadeOutResultFlash`'s at 92 in
  the same dump region, so `SingleModeMainTrainingCuttController` is the likely owner. `WaitForFixedUpdate`
  means that part is locked to the fixed step, so a `Time.timeScale` change reaches it only if the fixed
  step moves with it (C40).
- [ ] **A26 The dump cannot reach the classes this work needs, and the token index cannot name them
  either.** The 500 full class cap (`MAX_FULL_CLASSES`, `introspect.rs:60`) ran out at `introspect.log`
  L23504 while the `umamusume.dll` walk continued to L27524, so `SingleModeMainTrainingCuttController`,
  `SingleModeUtils`, `CutInTimelineController` and `TrainingParamChangeUI` produced hit lines only. Their
  members are invisible unless the method and field filters match them, and those filters
  (`introspect.rs:43-53`) contain no `cutt`, `cutin`, `frame`, `play`, `start`, `stop`, `update`, `skip`,
  `tag` or `tap`. Absence of `PlayTrainingCutt`, `_isTrainingCuttSkip`, `_trainingCuttStartFrame` and the
  `Update/FixedUpdate/LateUpdateTrainingCutt` trio from the dump therefore proves nothing. The token index
  in `hachimi-global-recon\pc_metadata_tokens.txt` holds no dotted token at all, so it can name a member
  but never its declaring class.
- [ ] **A27 Some of these classes are not in the `Gallop` namespace.** The dump prints a bare label for a
  class with an empty namespace or a nested type, and `SingleModeMainViewTagTrainingCutInPlayer` (L26716)
  and `<PlayTrainingCut>d__70` appear that way, while their neighbours print `Gallop.`. The mod resolves
  every speed class with `il2cpp_class_from_name(umamusume, c"Gallop", name)`
  (`AnimationSpeed.rs:1189`), so a hook on such a class logs "not present in this build" instead of
  failing loudly for another reason.

Bracketed status totals after this research edit: 11 `[x]`, 19 `[~]`, 47 `[ ]`, 3 `[latent]`, and 28
numbered fix order items.

## A28 to A30, what the run 9 allowlist dump actually delivered (2026-10-08)

The run 9 dump is 1,921,600 bytes over 27,463 lines and ends with `527 classes (27 from the allowlist), 15157 methods,
11238 fields written`. Line numbers below are in `hachimi\introspect.log`.

- [ ] **A28 The training cut-in names its own state, its phases, and the friendship test.**
  `Gallop.SingleModeMainTrainingCuttController` (dump L22929) has
  `get_IsPlayingCutt/0 -> bool()` and `set_IsPlayingCutt/1 -> void(bool)`, which is the game saying when a cut is
  playing, and the phase coroutines `TrainingAsync/1 -> IEnumerator(TrainingCommandId)`,
  `PlayTrainingCut/1 -> IEnumerator(CuttPlayInfo)`, `PlayScenarioTrainingCut/1 -> IEnumerator(CuttPlayInfo)`,
  `PlayTrainingSaboriAsync/1 -> IEnumerator(TrainingCommandId)`,
  `PlayTrainingCutEndAsync/3 -> IEnumerator(TrainingCommandId, bool, bool)`,
  `CleanUpCuttAndAssetUnload/0 -> IEnumerator()`, `CleanUpCutt/0 -> void()`, the per frame drivers
  `UpdateTrainingCutIn/0`, `FixedUpdateTrainingCutIn/0`, `LateUpdateTrainingCutIn/0`, the status panel doors
  `PlayInTrainingStatus/0 -> void()` and `PlayOutTrainingStatus/0 -> void()`, and
  `IsAutoPlay/0 -> bool()` next to `WaitTapAsync/0 -> IEnumerator()`. It also has
  `IsValidTag/2 -> bool(TrainingResultType, List<SupportCardData>)`, which is the regular versus friendship test this
  ledger has been missing, plus the friendship specific flash paths `LoadResultFlashTag/0`, `LoadResultFlashNormal/0`
  and `StartResultFlash/1 -> void(bool)`. `TryEnterStoryOnEndCutt/1 -> static bool(SingleModeEventPlayTiming&)` and
  `HasStoryOnEndCutt/0 -> static bool()` say a cut can hand off into a story event.
- [ ] **A29 The cut-in timeline reports its own length and progress, and it has an autoplay flag.**
  `Gallop.CutIn.Cutt.CutInTimelineController` (dump L26098) exposes `GetTotalTime/0 -> float()`,
  `GetTotalTimeCeil/0 -> float()`, `GetTotalFrameCeil/0 -> int()`, `get_CurrentFrame/0 -> int()`,
  `FrameToTime/1 -> float(int)`, `get_Speed/0 -> float()`, `get_DeltaTime/0 -> float()`,
  `get_WaitingTime/0 -> float()`, `get_DeltaWaitingTime/0 -> float()`, `get_IsWaitingUpdate/0 -> bool()`,
  `get_TargetFps/0 -> int()`, `get_IsMainCutt/0 -> bool()`, `get_IsEndless/0` with `set_IsEndless/1`,
  `get_DoNotOverTime/0` with `set_DoNotOverTime/1`, `set_SkipFrame/1 -> void(int)`, `UseResumeEndFrame/1 -> void(bool)`,
  `SetLoadFinish/0 -> void()`, and `get_IsAutoPlay/0 -> bool()` with `set_IsAutoPlay/1 -> void(bool)`. The game already
  knows how long the animation is and how far it has got, so a measurement should read that instead of inferring it
  from wall clock, and a skip should go through `set_IsAutoPlay` or `set_SkipFrame`, which are the game's own doors.
  `Gallop.SingleModeMainViewTrainingCutStatus` (dump L23742) has `PlayIn/4 -> void(float, int, bool, Action)`,
  `CoroutinePlayIn/2 -> IEnumerator(float, int)`, `PlayOut/2 -> void(bool, Action)`, `PlayPreIn/0`, `PlayEnd/0`,
  `GetIntervalOutBegine/1 -> float(float)` with `field OUT_BEGINE_INTERVAL [static const float]` and no setter
  sibling, `WillRankUpInHighSpeedMode/0 -> bool()`, `TargetFrameWillRankUp/1 -> bool(int)`, `ExistPlayingFrame/0`,
  `Skip/1 -> void(bool)` and `field _skipped [bool]`. `Gallop.TrainingParamChangeUI` (dump L24571) carries
  `field SCROLL_DURATION`, `GROUP_TYPEWRITE_INTERVAL`, `GROUP_TYPEWRITE_INTERVAL_HIGH`, `SEQUENCE_INTERVAL`,
  `SEQUENCE_INTERVAL_SUCCESSION` and `PLAY_SPEED_FOR_SKIP`, all `static const float`, beside the instance fields
  `_delay`, `_tapWait`, `_groupInterval`, `_sequenceInterval`, `_forceTapWait`. `PLAY_SPEED_FOR_SKIP` says the plate
  cascade already has a faster path the game uses when it is skipped.
- [ ] **A30 The friendship cut-in is a separate, shorter object.**
  `Gallop.SingleModeMainViewTagTrainingCutInPlayer` (dump L23709) has `PlayCutIn/2 -> void(List<SupportCardData>, Action)`,
  `PlayCutInOut/1 -> void(Action)`, `PlayButtonEffect/3 -> void(Transform, Vector3, Action)`, `DestroyButtonEffect/0`,
  `CreateLineEffect/1 -> Animator`, `PlayLineEffect/0`, `StopLineEffect/0`, `DestroyLineEffect/0`,
  `PlaySuccessTextEffect/0`, `SetResultFlashPlayer/1 -> void(FlashPlayer)`, `IsValidTag/1 -> static bool(List<SupportCardData>)`,
  and the constants `GRADATION_DEFAULT_ALPHA [static const float]`, `CUT_IN_MIN [static const int]`,
  `CUT_IN_MAX [public static const int]`, `TAG_TRAINING_CUT_IN_FLASH_SORT_OFFSET [public static const int]`. Each entry
  ends by invoking an `Action` the caller supplied, so anything that shortens these has to still call that callback or
  the coroutine that is waiting on it never resumes.

## B. Open items from the animation feature review

- [x] duplicate detour on `StoryViewController::GetTimeScaleByHighSpeedType` removed
- [x] static targets rejected in `AnimationSpeed::resolve_method`
- [~] `scale_frame_count` was put to use by `SetHighSpeedFrameCount`; that scaling is now removed with
  the rest of the story frame count path (A13, C34), so the helper has no caller left in this tree
- [ ] A2 overload matching
- [ ] A6 `Show` / `Hide` arity
- [ ] A7 `byref`
- [~] struct typed parameters still skipped in general; A5 shows which ones are actually safe. The
  bound is enforced by code now: `value_shape_for` refuses a `struct<...>` whose payload is over
  `MAX_INLINE_VALUE_BYTES = 4` in front of any value-shaped wrapper (item 25).

## C. Safety review findings (C1 to C51)

- [~] **C1 Calls through address 0.** Closed for `def_method_wrapper_fn!`,
  `impl_addr_wrapper_fn!` and both field accessor macro families. Still open: `get_orig_fn!`
  transmutes the 0 trampoline the interceptor returns for an uninstalled hook (234 sites, guard
  exists only in `UnityEngine_CoreModule/Time.rs:49-52`), and the 45 exported `WinHttp*` stubs plus
  `UnityMain` are `jmp qword ptr [rip + <orig>]` through `static mut _orig: usize = 0` while
  `src/windows/hook.rs:50-62` skips proxy init for this module name.
- [ ] **C2 No panic or SEH barrier at any hook boundary.** `catch_unwind` appears only in
  `src/core/gui.rs`; hook bodies call `unwrap()` on shared mutexes (poisoning makes every later
  call of that hook panic), `Hachimi::instance()` calls `process::exit(1)` when uninitialized
  (`src/core/hachimi.rs:139-143`), and `src/windows/hachimi_impl.rs:30` kills
  `UnityCrashHandler64.exe`, so an abort leaves no dump.
- [ ] **C3 Live Theater duplicate character.** `LiveTheaterCharaSelect.rs:11-13` skips
  `CheckSwapChara` and `LiveTheaterViewController.rs:7-17` returns
  `ChangeLive_onSuccess(this, null)`, a client side success branch with no server response.
  Depends on compiler generated symbol names that move with game updates.
- [ ] **C4 Injection point.** The mod sits at the game root as `cri_mana_vpx.dll`, shadowing the
  shipped CRI codec at `UmamusumePrettyDerby_Data\Plugins\x86_64\cri_mana_vpx.dll`, which never
  loads. No export forwarding, and Steam integrity verification does not remove a root file the
  game never shipped.
- [~] **C5 Unclamped speed layers.** `ui_animation_scale` allows 0.1..=1000.0 in the GUI - the
  Performance tab (`src/core/gui.rs:5891`) and the first time setup wizard, the only place a new
  user touches it and the range this delta widened from 0.1..=10.0 (`src/core/gui.rs:7337`) - with
  a code default of 1.0. Clamped in code now: `AnimationSpeed::refresh_config_mirrors` mirrors it
  through `normalize_ui_animation_scale` into `UI_ANIMATION_SCALE`, bounded by
  `MIN_UI_ANIMATION_SCALE..=MAX_UI_ANIMATION_SCALE` (0.1..=MAX_FACTOR = 20.0, non numeric input
  falls back to the neutral 1.0), and `DOTween/TweenManager.rs` reads that atomic instead of
  loading the config every tween tick. The wizard maximum therefore reaches DOTween as x20 -
  0.33 s of tween clock per 60 fps frame - instead of x1000 - 16.7 s. Verified by the new
  `cargo test --lib` case plus clippy clean for `x86_64-pc-windows-msvc`; no game run yet, and the
  sliders still offer more than the code honours (`Config snapshot:` reports the configured value,
  not the clamped one). Still open: `independent_time` is multiplied as well, although it is the
  channel DOTween leaves unscaled so UI keeps animating while the game is paused.
- [ ] **C6 Purchase path.** `PaymentUtility.rs:11-27` shows a Yes/No dialog that only queues,
  then calls the original `StartPurchase` unconditionally, and the Yes branch posts
  `PostMessageW(None, WM_CLOSE, 0, 0)` and sets `disable_gui_once`. Installed whenever the Steam
  overlay conflicts, which is the ordinary case.
- [~] **C7 Resolution by name plus argument count** outside `AnimationSpeed`, with hand written
  wrapper signatures at each site. Closed for the three `StoryManager` statics `HighSpeedSetting`
  calls through (`resolve_static_method`, item 25, committed in `75f2147`); the roughly 250 other sites
  still use it, which is why the item stays `[~]` and not `[x]`.
- [ ] **C8 Mod state keyed on raw object addresses** while IL2CPP recycles freed addresses
  (`Sqlite3/Connection.rs:97,102-115` and others).
- [ ] **C9 Unchecked dereferences of game returned values** (`AssetBundle.rs:79-80` and others).
- [ ] **C10 Asset patch identity guard disabled** for the Global target while
  `AssetBundle.LoadAsset_Internal` is hooked for every asset load.
- [ ] **C11 Check then use race** in the live translation apply path
  (`Text.rs:96-117`, `TextMesh.rs:38`).
- [~] **C12 `Time::set_timeScale` overwrite** is a simulation lever rather than an animation lever.
  `Time::apply()` wrote the configured `time_scale` straight into Unity, and `SceneManager`
  called it from `ChangeViewCommon` after every view change, so a `time_scale` of 2 replaced a
  game pause (0) or a game fast forward (4 became 2) and did it once per view change: the
  pre-fix reproduction ends with `timeScale` 2 and 6 writes after one pause plus five view
  changes, and 20 writes for 20 view changes of an unchanged config. `apply()` now runs once
  per config value behind an `APPLIED_LEVER` marker, derives its single write from
  `GAME_REQUESTED` (the value the game last asked for, recorded by the write hook) through the
  same `apply_time_scale`, and returns `None` - no write at all - for a game value at or below
  1.0, so pauses, slow motion and the game's neutral 1.0 are never overwritten; dropping the
  lever back to 1.0 writes the game's own value once. `ChangeView` no longer calls it. Verified
  by the reproduction in `Time.rs` (`cargo test --lib UnityEngine_CoreModule::Time`, 8 passing)
  against the pre-fix scratch reproduction; that reproduction stood on the test module's `Sim` copy of
   the hook (C41), and the same cases now run `scale_game_write`, `claim_lever` and `plan_write`, the
   functions the detour and `apply()` call. No game run yet, so it stays `[~]`. Consequence:
  the option is now purely a multiplier on the game's own values above 1.0 and writes nothing
  by itself, which is the C12 safety line; a run has to confirm the log line
  `Time::apply: game <v> x lever <l> -> <w>` appears at most once per config change.
- [latent] **C13 Static duration constant re-assertion** in `AnimationSpeed` writes
  `static readonly` constants every `apply()` and adopts values the game wrote. Inert on this
  client: 0 of 61 fields resolve, all 61 rejected as literal or non static. C36 re-gated the
  re-assertion to one pass per factor change.
- [ ] **C14 Options that do nothing on this client** while still offered in the GUI:
  `hide_now_loading` (A6) and `ui_loading_show_orientation_guide`.
- [ ] **C15 Coroutine aborts are class wide** because `symbols.rs:266-299` patches `MoveNext` on
  the compiler generated enumerator class.
- [ ] **C16 `on_game_initialized()` runs twice** when `ui_scale != 1.0`.
- [ ] **C17 The input layer is suppressed** by 11 hooks during free camera capture, gated on a
  global mutable scene flag with two reset points (`InputSystemManager.rs:26-91`).
- [ ] **C18 Unauthenticated local control plane.** `src/core/ipc.rs:13-19` binds 0.0.0.0:50433
  when `ipc_listen_all` is true, and the plane can call `SoftReset`, `StoryGotoBlock` and
  `ReloadLocalizedData`. Currently `enable_ipc: false`.
- [ ] **C19 Third party sourced URLs and text reach game surfaces.** `WebViewManager::GetUrl`
  substitutes `news_url` from the downloaded translation repo config, and
  `UnityEngine_CoreModule/Application.rs:49-55` suppresses `Application::OpenURL` when the mod
  WebView opens. Full URLs including query parameters are written to the log
  (`src/windows/webview.rs:270`) and every gacha URL is stored in an unbounded map.
- [ ] **C20 Native SQLite interception of the game's encrypted databases.**
  `src/core/hachimi.rs:19-46,345-357` hooks `sqlite3_open_v2` and `sqlite3_key`, keeps the raw
  key in `RETRIEVED_RAW_KEY` (`src/il2cpp/sql.rs:12-13`), and applies it to whichever handle next
  consumes a global one-shot flag.
- [ ] **C21 SQL bind ordinals can be recorded against the wrong column** because the parameter
  counter only advances for `identifier = ?` predicates (`Sqlite3/Connection.rs:63-69,102-115`,
  `src/il2cpp/sql.rs:137-154,793-822`). Substitution inside the game's data path, not a UI layer.
- [ ] **C22 `story_tcps_multiplier` compounds** on repeated loads of the same asset instance
  (`StoryTimelineData.rs:104-109` reached from both `AssetBundle.rs:59` and
  `AssetBundleRequest.rs:17`) with no applied marker, GUI range 0.1..=1000.0, live value 1000.0.
- [ ] **C23 Story block length recomputation can shorten a block** (
  `StoryTimelineData.rs:340-341` writes `StartFrame + newClipLength + 1` with no
  `max(originalBlockLength, ...)`), cutting trailing frames of longer non text tracks.
- [~] **C24 Story choice auto select applies the same factor twice.**
  `StoryChoiceController.rs:41-51` and `StoryViewController.rs:17-18` both apply a number derived
  from `0.75 / story_choice_auto_select_delay`, and the getter is only scaled while the flag
  `CheckChoiceAutoTap` raises is up, so the second site scales the scale the first site's
  accumulator is measured against: one auto tap request carries the factor twice. That double
  application is the item's root cause and it is still open. What is fixed here is the clamp and the
  record of the live numbers. Both sites now read one clamped mirror (`STORY_CHOICE_AUTO_SELECT_MULT`,
  written by `refresh_config_mirrors`, NAN for inert) instead of loading the config on a story path,
  the delay keeps its code floor of 0.1, and each half is capped by the quantity it touches:
  `MAX_STORY_CHOICE_AUTO_SELECT_MULTIPLIER` = MAX_FACTOR = 20 on the accumulator increment, which is
  seconds, and `MAX_STORY_CHOICE_AUTO_SELECT_TIME_SCALE` = MAX_TIME_SCALE = 5.0 on the value the
  getter hands the story timeline, through `scale_read_time_scale`. The single ceiling the earlier
  clamp put on both halves was itself a defect: MAX_FACTOR on a time scale left the story clock at 7.5
  for a game scale of 1.0 at the Config Editor's left end, and 20 with a hand edited config, against
  the 5.0 every layer that reaches that scale must enforce. Live numbers now, at
  delay 0.1: increment x7.5, story time scale 1.0 -> 5.0, so at most 37.5 along one request. The
  pre-fix `0.75 / 0.0001` = 7500 is reachable from neither the slider nor config.json (0, -1.0, NAN,
  1e-8, 1e6 all come out inert or bounded). Verified against the reproduction in `AnimationSpeed.rs`
  (`cargo test --lib`: `story_choice_auto_select_multiplier_is_bounded_at_both_sites` and
  `the_story_choice_getter_half_is_bounded_by_the_time_scale_ceiling`): a game 0.0 and 0.5 pass
  through, a scale the game holds at 8.0 is not pulled down, 2.0 x 7.5 tops out at 5.0, a delay at or
  above the game's own 0.75 leaves the story clock at 1.0, and an unwritten mirror is inert on both
  halves. No game run yet, so it stays `[~]`; the lines a run has to read are `AnimationSpeed:
  StoryViewController::GetTimeScaleByHighSpeedType 1 -> 5` and `AnimationSpeed:
  StoryChoiceController::CheckChoiceAutoTap <increment> -> <scaled>`, and installed is not the same as
  called (A4). Still open: whether the two paths compound in the live client depends on whether
  `_choiceAutoSelectWaitTime` accumulates the game's scaled delta, which only a run can answer; the
  0.75 trigger constant is assumed rather than read from the client (`AnimationSpeed.rs:210-213`);
  `IS_CHECKING_CHOICE_AUTO_TAP.swap(false)` consumes the flag on the first nested getter call; only
  `StoryViewController::GetTimeScaleByHighSpeedType/0` is hooked and its `/1 -> static
  float(struct<StoryTimelineController.HighSpeedType:4B>)` overload is not (A5); and
  `StoryTimelineController::GetTimeScaleByHighSpeedType/1 -> static float(bool)` is hooked as
  measurement only (item 21).
- [ ] **C25 About 5.5 s of work runs inside `DllMain` under the loader lock**: 198 hook install
  requests, a 1.9 MB dump written into Program Files, native sqlite hooking, window subclassing
  plus a CBT hook, a Discord IPC pipe connect that fails, and `TerminateProcess` of another
  process by image name with no ownership check. Batch arming (`865935f`) removes the 24 ms per hook
  from this list but not the dump, the sqlite hooking, or the window work.
- [ ] **C26 Outbound update check plus unsigned installer execution path**, live by default,
  from inside the game process (`src/core/hachimi.rs:560-571`, `src/core/updater.rs:151-192`).
- [~] **C27 `disabled_hooks` matches bare wrapper names** that are not unique across modules
  (`Hide` appears in 4 files, `Update` in 3), `new_hook!` logs an install line before the null
  check, and `Interceptor::hook` keys on the hook function address so a reused wrapper would call
  the first target's trampoline. Repaired for this fork's own new hook in `75f2147`: the story time
  scale hook is `StoryTimelineController_GetTimeScaleByHighSpeedType`, so one `disabled_hooks` entry no
  longer silences upstream's `StoryViewController` hook that carries the same bare name. The `Hide` and
  `Update` collisions, the install line logged before the null check and the key on the wrapper address
  are untouched.
- [ ] **C28 Race replay seeking in Global leaves used-skill state unrepaired** because the resync
  stage returns early unless region is Japan (`RaceManagerReplayBase.rs:71-72,110-112`).
- [ ] **C29 All mod state lives inside the Steam managed install tree** (`hachimi\config.json`,
  `hachimi.db`, `introspect.log`, generated story dictionaries, `<bundle>.a.png` beside shipped
  bundles), and extreme speed values are re-applied on the next load of any copy.
- [ ] **C30 No structured resolution report** and a failed file logger is silent
  (`src/windows/log_impl.rs:9-24`).
- [latent] **C31 Android only hooks that invert the game's legality checks**
  (`Cute_Core_Assembly/Device.rs` `IsIllegalUser`, `SafetyNet.rs:16` passing the success callback
  as the failure callback). Not compiled into the Windows deliverable
  (`src/il2cpp/hook/mod.rs:198-199`), but they ship in this tree.
- [latent] **C32 No wrapper in this tree declares the trailing `const MethodInfo*`** that IL2CPP
  generates as the last parameter of every managed method, so a detour that calls its trampoline
  leaves that slot holding whatever register value it had. Zero of the ~200 wrappers pass it
  (`grep` for a `MethodInfo` parameter returns nothing), which is the convention across the whole
  IL2CPP hooking ecosystem, and three logged sessions with 200 armed hooks reached training, races
  and result screens without a single related fault. Recorded as a known property, not a defect to
  chase: it only matters for a wrapper that would dereference method metadata, and none does.
- [ ] **C33 `get_orig_fn!` is a `Mutex<HashMap>` lookup with `unwrap()` per call**
  (`src/core/interceptor.rs:113-119`). Every detour pays it, and the observe only probes add thirteen
  more candidates on paths the story code walks per frame. Cost is tens of nanoseconds and is not
  what this fork is losing time to, but the `unwrap()` on a shared lock inside an `extern "C"` frame
  is the same shape C2 warns about. Cached trampoline handles are the fix if this ever lands on a
  measured hot path.
- [ ] **C34 The story skip frame values are a chain, not a duration.** `SkipFrameCount` and
  `SkipMotionFrame` receive the same number, and what the fork hands to the first is fed straight
  into the second by the game, so a hook on both applies the factor twice along one request
  (`133 -> 733`, then `733 -> 1333` in run 5). The values rise monotonically inside a scene
  (133, 243, 161, 263, 691, 163, and 76, 182, 313, 448, 583 in run 4), which reads like a frame
  target inside the timeline rather than a wait length, and pushing a target past the block the
  timeline stands in stops advancement: the wait count froze at 96 and `IsHighSpeedMode` spun at
  180 calls a second. Any future work on these paths has to scale at most one of them, has to know
  which one the game forwards, and has to bound the result against the block length rather than a
  multiplier. The scaling is removed; both hooks stay as counters. The same decision now covers the
  two story group scalings left on this path, `StoryTimelineController::SetHighSpeedFrameCount` and
  `GetNextFrameCount_HighSpeed`: the int either one handles is a candidate index into the readonly
  `_highSpeedFrameCountArray`, and `scale_frame_count` floors a positive result at 1, so dividing
  that index replaces the entry the game selected instead of shortening a wait: at 1.5 the mapping
  is 1Ã¢â€ â€™1, 2Ã¢â€ â€™1, 3Ã¢â€ â€™2, 4Ã¢â€ â€™3, 5Ã¢â€ â€™3, 6Ã¢â€ â€™4, so neighbouring requests land on the same slot, and at the
  >= 5.5 magnitude run 5 produced every index from 1 to 6 collapses onto slot 1. Both paths now hand
  their values through untouched and only count and log them.
- [~] **C35 The `time_scale` layer had no ceiling, multiplied the same quantity twice, and inverted
  the game's own sub-1 writes.** `Time.rs` did `value *= config.time_scale` on every value the game
  wrote while nothing on that path consulted `MAX_TIME_SCALE`: a slow motion write of 0.5 came out
  1.5 at lever 3.0, the story group's `scale_time_scale` (capped at 5) and that hook multiplied the
  same quantity twice so the logged `StoryTimeline_getTimeScaleAfterEndStory 1 -> 5` followed by a
  game write reached 25, and a read-modify-write loop climbed 4, 10, 20 Ã¢â‚¬Â¦ 640 in eight frames.
  `config.json` is deserialized unbounded and `apply()` passed the raw value through the icall.
  Now `AnimationSpeed::apply_time_scale` is the one arithmetic every layer shares: a value at or
  below 1.0 (the game's 0 pauses and its slow motion) is never multiplied, a value above it is raised
  and clamped to `MAX_TIME_SCALE`, the lever itself is clamped by `normalize_time_scale` to
  `MIN_TIME_SCALE..=MAX_TIME_SCALE` and mirrored in `TIME_SCALE`, and the Config Editor slider offers
  no more than the ceiling. Verified by arithmetic against the reproduction (0.5 stays 0.5, story 5
  then lever 5 stays 5, config 1000 writes 5, the loop saturates at 5, the neutral lever changes
  nothing); no game run yet, so it stays `[~]`. Known consequence: with the option on, the game's own
  writes of 1.0 pass through untouched, so `time_scale` acts on values above 1.0 only; the mod's own
  absolute write that used to complete the picture is gone (see C12).
  The consequence was wider than this line said, and it is why C39 exists: `scale_time_scale` delegated
  to the same rule, so the three story getters stopped raising the game's 1.0 as well. The arithmetic is
  split now, `apply_time_scale` is the write half described above and `scale_read_time_scale` is the read
  half a getter uses.
- [x] **C36 `AnimationSpeed::apply_if_dirty` re-ran the whole `apply()` every game frame.**
  `WAS_SCALED` latched on the first non-neutral write, so `GameSystem_Update` paid 61
  `il2cpp_field_static_get_value` reads plus 61 writes, three `config.load()` (`factors()`,
  `HighSpeedSetting::apply`, `StoryTimelineController::apply_config`), an `ENTRIES` lock taken *before*
  the empty bail out, and the whole `HighSpeedSetting` getter chain with its log line, every frame, for
  values that had already been written. Now `APPLIED_FACTORS` holds the factor each group's fields were
  last written at (NAN means never written, which is the state a neutral factor asks for), `plan_group`
  compares it with the configured factor, a group is rewritten once per factor change, the bail out runs
  before the lock, `factors()` became `refresh_config_mirrors` plus `mirrored_factors` so the write loop
  reads the mirrors, and `apply_if_dirty` returns before any of it when nothing is dirty. A group whose
  fields had no baseline in a pass keeps its marker unset so the next pass retries it - one pass per view
  change or config change, not per frame. Verified at the time against a reproduction in
  `AnimationSpeed.rs` that was a copy of the pass rather than the pass: the test module's `Sim` ran its
  own `apply`, its own write loop and its own `reads += 1` and `writes += 1`, so the number it printed,
  300 ticks at transition x2 going from 18300 static reads + 18300 writes to 13 + 13, is that model's
  arithmetic (300 ticks over all 61 fields against 300 ticks over one group) and not a measurement of
  `apply`, and the model held none of what a view change really pays: `HighSpeedSetting::apply` and
  `StoryTimelineController::apply_config`, each a `config.load()`, `refresh_config_mirrors`'s own read,
  and the `ENTRIES` lock. C41 removed the model. The claim now rests on the functions the shipped pass
  runs (`mirror_config`, `plan_pass` and `finish_pass`, driven by `cargo test --lib`: 300 ticks ask for
  one rewrite pass, a neutral config asks for none, only the group whose factor changed is ever due, a
  factor back to 1.0 asks for the restore once, a second factor still scales the game's baseline and not
  our own write through `baseline` and `scale_value`) and on the counters `apply` charges itself. Run 7
  read that line: passes 1 to 6 charged 4, 7, 10, 13, 16 and 19 config reads and one entry lock each, 0
  table passes, 0 field reads and 0 field writes, and no pass 64 line appeared in 534 s. What a run has to read is the `AnimationSpeed apply pass N: a config
  reads, b entry locks, c table passes, d field reads, e field writes` line; on this client `d` and `e`
  stay 0 until the duration fields resolve (C13), so the per field half of the old headline has never
  been measured anywhere. Known consequence: a group at an unchanged factor is no longer looked at, so
  a class whose static constructor runs mid scene, or a value the game reassigns at runtime, keeps its
  own number until the factor changes or the option is turned off. The way to hold such a field without
  paying per frame is a detour on the method that writes it, not a re-assertion loop - the tradeoff C13
  describes.

- [x] **C37 `AnimationSpeed::init` applied the speed options from inside the batch window.** Its
  last statement was `apply()`, and `init` runs between `Interceptor::begin_batch` and
  `finish_batch` (`src/il2cpp/hook/mod.rs:272`, `:297`, `:307`, reached through
  `src/il2cpp/hook/umamusume/mod.rs:368`), which sits under `DllMain` and the `LoadLibraryW`
  wrapper (`src/windows/main.rs:62-73`, `src/windows/hook.rs:64-71`) with the loader lock held,
  before the game finished initialising and while no hook is armed. `apply()` reaches game code
  through `HighSpeedSetting::apply`: `StoryManager::GetMaxHighSpeedType`,
  `GetSavedHighSpeedSetting` and `SaveHighSpeedType` (the persisting one), plus
  `SaveDataManager::instance`, `SaveDataManager::get_SaveLoader` and
  `ApplicationSettingSaveLoader::get_TrainingHighSpeedType` / `set_TrainingHighSpeedType`. The
  field write loop itself was inert on this client (C13: 0 of 61 fields resolve), so the game calls
  under the loader lock were that getter chain and the saving write. `init` now ends with
  `refresh_config_mirrors` - atomics and one `config.load()`, no game state - plus `mark_dirty()`,
  so the first write is the one the first `GameSystem_Update` tick performs through
  `apply_if_dirty` (`GameSystem.rs:49`), the deferred shape `Time::init` already uses.
  `SceneManager::ChangeView` still runs the apply before the first view change reads its fade
  constants, and the `HighSpeedSetting` raise retries at the next view change if `SaveDataManager`
  had no instance on that first tick. Verified by `cargo check` for `x86_64-pc-windows-msvc` plus
  `the_game_tick_path_reaches_the_pass_once_per_config_change` (300 calls through the shipped
  `mark_dirty` / `apply_if_dirty` pair reach `apply` once), and by the `AnimationSpeed apply pass N:`
  totals `apply` now charges itself (C41). The test that used to stand here,
  `the_install_window_writes_no_field_and_the_first_game_tick_does`, was the test module's `Sim`
  counting its own copy of the pass and it is gone. `cargo clippy` for `x86_64-pc-windows-msvc` could
  not be run from this machine; clippy 0.1.98 is installed for this toolchain and `cargo clippy --target
  x86_64-pc-windows-msvc --all-targets -- -D warnings` is clean in this tree (C42 corrects this claim); run 7 shows the
  sequencing, pass 1 is stamped 21:12:48.304 against `Hooking finished:` at 21:12:48.009, and the
  `HighSpeedSetting:` snapshots land after it.

- [~] **C38 The result screen auto skip serialises every result part behind one process-wide flag,
  and the exit that drops a request logged nothing.** `SingleModeResultContentBase.rs:25` holds one
  `static IN_SKIP: AtomicBool` for the whole process, while the call it guards is per instance:
  `skip_fn(this)` finishes the tween chain of the result part that asked. So while one part is inside
  `SkipFadeInTween`, `SkipGuard::try_enter` refuses every other part on the same screen and the hook
  returned without writing a line. The only two signals a run had were the entry line and the
  `SkipFadeInTween unavailable` warning, and a dropped request writes both of them exactly the way a
  finished chain does - which is how run 3 read "5 entries, 0 unavailable" as proof the skip worked
  end to end. Reproduced against a mock game: a run where a second part loses its request and a run
  where both parts are skipped write byte identical logs, and the ledger's arithmetic reports 2
  finished chains for a run that finished 1.
  Now `SKIP_GUARD_CONTENDED` counts every dropped request and each one gets its own line,
  `auto_skip_result_screens: skip guard busy, request N dropped (asked by <ptr>, held for <ptr>)`,
  first six then a totals line every 4096 like the story counters, and `IN_SKIP_OWNER` records which
  instance holds the guard, so the pointers say whether a contention is the completion callback
  re-entering the same part (expected, harmless) or a different part that lost its skip. The count
  turns the log into an equation: `entries - unavailable - contended` is the number of chains this
  hook finished. Verified by the reproduction (the two identical logs became different, the arithmetic
  matches the ground truth in both runs, 5000 contended requests cost 7 log lines and count 5000, a
  panic inside the guarded call still releases the guard) plus `cargo test --lib` (48 pass) and clippy
  clean for `x86_64-pc-windows-msvc`; the `aarch64-linux-android` leg has since run clean here too (C42)
  and nothing added is platform specific. No game run yet, so it stays `[~]`. What it does not change is the
  serialisation itself: one chain at a time stays the conservative choice until a run shows how often
  real result screens contend, because a per instance guard admits the two-part ping-pong the guard
  exists to stop. The line a run reads is the contention count beside the entry count.

- [x] **C39 The rule C35 introduced disabled `story_speed` on the story path and no record said so.**
  `apply_time_scale` leaves a value at or below 1.0 exactly as it is, `scale_time_scale` delegated to it,
  and `scale_time_scale` is what the three installed Story getters scale by
  (`StoryTimeline_getTimeScaleEventWipe`, `StoryTimeline_getTimeScaleAfterEndStory`,
  `SingleModeUtils_GetCutTimeScale`, `AnimationSpeed.rs:998-1000`). That is the only story scaling this
  fork measured live: `StoryTimeline_getTimeScaleAfterEndStory 1 -> 5` in runs 1 and 2 (A4 and the Run 2
  and again in run 3, and the rule turned it into `1 -> 1`, which `hit` does not even log because
  the value did not change. With C13 (0 of 61 duration fields resolve) and the two Story duration getters
  installed with no call (A4), nothing in `Group::Story` had a measured effect, while `story_speed`
  stayed a slider (`gui.rs:5916-5919`), a name on the `Config snapshot:` line and a key in all ten
  locales, which is what AGENTS section 9.5 and C14 forbid. C35's own consequence note named `time_scale`
  only. Reproduced on the fork's own arithmetic before the fix: with the story mirror at 5.0,
  `scale_time_scale(1.0, Group::Story)` returned 1.0.

  The reason for the rule is a value the game is writing: 0 is a pause, a value under 1 is slow motion and
  1.0 is the speed the game chose to run at, and C12 forbids the mod raising any of them. A getter is the
  other direction: it hands back the scale the timeline steps its own clips by, and the 1.0 there is the
  neutral playback speed a speed up option exists to raise. The committed build before C35 scaled it
  (`(value * factor).min(MAX_TIME_SCALE).max(value)`) and measured 5 out of it in three runs, so this is a
  regression being restored rather than a new lever.

  Fix: the two directions are two functions. `apply_time_scale` keeps `value <= 1.0` and is documented as
  the write half; `Time.rs`'s detour, its `plan_write` and its tests are untouched. The new
  `scale_read_time_scale` (`AnimationSpeed.rs:309-315`) protects only a value under 1.0, passes a scale
  the game already holds above `MAX_TIME_SCALE` through instead of pulling it down (AGENTS section 5: time
  scales only go up), and caps the raise at `MAX_TIME_SCALE`. `scale_time_scale` (:319-321) calls it, and
  that is the whole change to the hook path. The one ceiling still reconciles both levers: a getter
  raising 1.0 to 5 and a `time_scale` lever of 2 on the game write stop at 5.

  Verified here: the reproduction now returns 5.0, a stored 0.0 and 0.5 still pass through, a scale the
  game holds at 8.0 stays 8.0, `MAX_FACTOR` on a 1.0 scale comes out at `MAX_TIME_SCALE`, the neutral
  factor still changes nothing, and the write half still refuses 1.0 and 0.5 and still pulls 8.0 down to
  5.0 (`cargo test --lib`, 47 pass; `cargo check --all-targets` and `cargo build --release` clean). Run 7 read the line this item asked for:
  `AnimationSpeed: StoryTimeline_getTimeScaleAfterEndStory 1 -> 5`, with `story 10` on the snapshot line.
  What only a run can answer: whether a `story_speed` above 1.0 prints
  `AnimationSpeed: StoryTimeline_getTimeScaleAfterEndStory 1 -> 5` (or the EventWipe or GetCutTimeScale
  spelling) on this client, and whether raising that story scale removes wall clock time without
  disturbing story advancement, the run 5 shape this ledger is careful about. The other reading of this
  item, that the option stays off the story path and gets a C14 note like `hide_now_loading`, is one line
  away: point `scale_time_scale` back at `apply_time_scale` and `story_speed` is inert again.

  Correction, found in the scoped checkup of this change set: the sentence above that the write half
  "still pulls 8.0 down to 5.0" recorded a defect as expected behaviour, and the split is what dropped
  it. HEAD's one shared helper ended `(value * factor).min(MAX_TIME_SCALE).max(value)`; the new write
  half ended at `.min(MAX_TIME_SCALE)`, so at any lever above 1.0 `apply_time_scale(8.0, 2.0)` came out
  5.0 and both writers of `Time.timeScale` take that arithmetic, the `set_timeScale` detour through
  `scale_game_write` and the deferred config write through `plan_write`. The mod therefore wrote 5.0
  over a game value of 8.0, the thing the `plan_write` docstring says the design avoids and what the
  read half refuses, so the two halves of one helper disagreed with each other and with AGENTS
  section 5. Fix: the never lower floor is back on the write half (`AnimationSpeed.rs:401` is
  `.min(MAX_TIME_SCALE).max(value)` again), its comment now states the floor instead of the opposite,
  the test that baked the lowering in asserts a game scale above the ceiling passes through, and
  `Time.rs` gained `a_game_scale_above_the_ceiling_is_not_pulled_down`, which runs both writers. The
  ceiling still binds every raise the mod makes, so nothing compounds: a game 4.0 at lever 2.0 is 5.0,
  a second raise from 5.0 stays 5.0, `scale_game_write(5.0, 5.0)` is 5.0, and a value at or below the
  ceiling can never come out above it. The only scale above `MAX_TIME_SCALE` that can reach Unity
  through these paths is one the game itself asked for. Verified here: `cargo test --lib`, 59 pass,
  0 warnings, and the reproduction fails without the floor (`left: 5.0, right: 8.0`). No game run yet,
  so this stays `[~]`.
  What only a run can answer: whether this client ever holds `Time.timeScale` above `MAX_TIME_SCALE`
  (the mod's own raise cannot put it there), which a `Time::set_timeScale call` line shows, and whether
  leaving such a value alone changes the wall clock numbers this ledger measures.

- [~] **C40 `apply_time_scale` scaled downward too, and the Config Editor offered the range that reaches it.**
  C35's clamp floors the lever at `MIN_TIME_SCALE = 0.1`, `normalize_time_scale` kept it there and the
  Performance slider used those same constants as its left end (`gui.rs:5900`), so the option could be
  set below 1.0. The ceiling arithmetic only caps upward, `(value * factor).min(MAX_TIME_SCALE)`, so a
  sub 1.0 lever multiplied a value the game had put above 1.0 back under it: `apply_time_scale(4.0,
  0.1)` came out 0.4, and both paths that reach Unity take that arithmetic, the `set_timeScale` detour
  and the deferred config write through `plan_write`. That is the inversion C35 exists to refuse,
  against AGENTS section 5 (time scales only go up, and are capped) and against the note in `Time.rs`
  saying the design avoids a lever that slows the game's own fast forward down. Nothing in the tree
  asked the question: the `Time.rs` state machine tests exercised levers 1.0, 1.05, 1.2, 2.0 and 3.0
  only, and no test covered `normalize_time_scale` at all.

  Fix: the lever's floor is its neutral value. `MIN_TIME_SCALE` is 1.0, so the clamp every consumer of
  the lever reads the config through (`refresh_time_scale`, `refresh_config_mirrors`) and the slider
  range that reuses those constants both land on an inert 1.0 for anything below it, a hand edited 0
  included. The arithmetic refuses a factor at or below 1.0 on both halves, `apply_time_scale` and
  `scale_read_time_scale`, which is the floor `story_choice_time_scale_factor` already applied to the
  story choice lever.

  Verified here: the reproduction stops at the game's own number, 0.4 became 4.0 at a lever of 0.1 on
  the hook path and on `plan_write`, the slider's left end lands on 1.0 and `normalize_time_scale(0.0)`
  is 1.0, while everything the earlier fixes established still holds (a game 1.0 is not pushed, a 4.0 at
  lever 2.0 is 5.0, a stored 0.0 and 0.5 pass through, a scale the game holds at 8.0 is not pulled down,
  the neutral lever writes nothing): `cargo test --lib`, 50 pass, including the two new cases
  `a_time_scale_lever_below_one_never_lowers_a_time_scale` and
  `a_lever_below_one_does_not_slow_the_games_fast_forward`. No game run yet, so it stays `[~]`. What
  only a run can answer: whether a player config left holding a lever below 1.0 had been slowing a real
  Gallop write above 1.0 before this narrowed the range; `Config snapshot:` prints `time_scale`, so a
  run reads that directly.

- [~] **C41 The C36 verification numbers came from a hand written simulator, and the tests the ledger
  cites never ran.** One root cause, two breaks. `AnimationSpeed::apply` logs only what it changes
  (`AnimationSpeed: Class.Field a -> b (xN)` and `shortened N animation duration fields`), and on this
  client neither line fires because `init` resolves 0 of 61 duration fields (C13), so nothing in the
  tree printed what a pass *costs*. The cost C36 removed was therefore measured in the test module:
  `AnimationSpeed.rs` kept a `Sim` with its own `apply`, its own plan array, its own write loop and its
  own `self.reads += 1 // il2cpp_field_static_get_value` and `self.writes += 1`, and `Time.rs` kept a
  `Sim` whose `game_write` restated the detour and whose `apply` restated the marker check. The
  headline (18300 static reads + 18300 writes down to 13 + 13) is that copy's arithmetic over 300
  ticks, and the copy holds none of what a view change actually pays: `HighSpeedSetting::apply` and
  `StoryTimelineController::apply_config`, each a `config.load()`, `refresh_config_mirrors`'s own read,
  and the `ENTRIES` lock. Alongside it, `.github/workflows/clippy_check.yml` had no test step at all,
  so clippy compiled the `#[cfg(test)]` modules and never ran them, while AGENTS.md still stated that
  no unit tests for hook code exist.

  Fix, three parts. (1) The pass counts itself: `APPLY_PASSES`, `APPLY_TABLE_PASSES`,
  `APPLY_CONFIG_READS`, `APPLY_ENTRY_LOCKS`, `APPLY_FIELD_READS` and `APPLY_FIELD_WRITES`, charged at
  the sites that do the work (the top of `apply`, the entry lock, `refresh_config_mirrors`,
  `read_static`, `write_static`, and the two game setting halves through `AnimationSpeed::note_config_read`),
  printed by `report_pass` on every exit path with the fork's first 6 then every 64 pattern. They sit
  inside `apply`, which `apply_if_dirty` reaches once per config change, so a quiet game tick still
  pays one atomic swap and nothing else. (2) The sequencing the copy duplicated is now code the pass
  runs and the tests call: `mirror_config(&Config)` split out of `refresh_config_mirrors`,
  `plan_pass(factors)` for the three group decisions and `finish_pass(rewrite, factors, no_baseline)`
  for the markers, and in `Time.rs` `scale_game_write(value, lever)` for what the detour does with one
  game write and `claim_lever(lever)` for `apply`'s marker gate. Both `Sim` structs are gone. What a
  test process still cannot reach is written down where it stands: an il2cpp field read or write, the
  `ENTRIES` table, the `set_timeScale` icall read and write, and `Hachimi::instance()`, which ends the
  process when it is asked for before init, so `apply` now stops at a `Hachimi::is_initialized()` guard
  instead of walking into a config read with no singleton behind it. (3) CI gained a `unit_tests` job
  on `windows-latest` running `cargo test --lib`: the crate builds only for Windows and Android, so
  these are host tests and cannot ride the cross compiling clippy jobs. AGENTS.md section 4 now states
  what the tests drive and what they cannot prove, and section 7 lists the apply totals line.

  Verified here: `cargo test --lib`, 50 passed / 0 failed, with the eight `AnimationSpeed` cases that
  replaced the `Sim` tests and `claim_lever_holds_one_pass_per_lever_value` in `Time.rs` driving
  `mirror_config`, `plan_pass`, `finish_pass`, `mark_dirty` and `apply_if_dirty`, `scale_game_write`
  and `claim_lever` directly; no test module copy of either pass is left to agree with itself.
  `cargo check --all-targets` and `cargo build --release` finished with no warnings. The neutral
  default asks for no rewrite at all, 300 game tick calls reach `apply` once, only the group whose
  factor changed is ever due, a factor back to 1.0 asks for the restore once, a group whose fields had
  no baseline is asked again, and the apply line prints 6 detail lines for a short run then one every
  64 passes.

  What is still a model, said so the next reader does not over read it: `HighSpeedSetting.rs` and
  `StoryTimelineController.rs` keep a `Sim` in their test modules. Both stand in for the il2cpp half
  only - `StoryManager`'s getter and setter chain, `SaveDataManager` and `ApplicationSettingSaveLoader`,
  `StoryTimelineController`'s static field - all of which need a live `this` or a resolved class, and
  `StoryTimelineController`'s `reads_as_high_speed` is the test's own guess at what the game's
  `IsHighSpeedMode` answers. Their decisions do run through shipped functions (`plan_pass`,
  `raise_value`, `restore_value`, `observe`, `already_written_for_state`) and both modules already
  count their own writes in the shipped code (`HighSpeedSetting` totals what a restore put back,
  `StoryTimelineController` has `HIGH_SPEED_WRITES`), so the write lists in those tests are a model's
  counts and the lines those counters print are a run's. The remaining work on this item is to put
  those two write halves on the same footing as `plan_pass`, which needs the game's own objects.

  What only a run can answer: the `d field reads, e field writes` half of the apply line, so any per
  field cost number at all. The 13, 39 and 9 fields are counts read off `FIELDS`, not measurements, and
  on this client the table is empty (C13), so a run is expected to read the pass as config reads plus
  one lock per due group and no field call. Whether the `Hachimi::is_initialized()` guard ever fires in
  game is a run question too. And the lint gate on the new code: clippy 0.1.98 is installed for this
  toolchain, so the gate ran here rather than waiting on CI. Both CI legs, `cargo check --all-targets` and
  `cargo build --release` finished clean on 2026-10-08, the Android leg included (C42). A game run is still
  what the cost half of this item needs.

- [~] **C42 The `aarch64-linux-android` leg was never compiled for the real crate, and the reason every
  entry gave for it was false.** AGENTS section 1 and 4 and `.github/workflows/clippy_check.yml:39-48` gate
  the fork on clippy `-D warnings` for `aarch64-linux-android`, and this series' most platform sensitive
  change is exactly the surface only that leg compiles: `src/il2cpp/mod.rs:14-17` put `pub mod slot_table`
  and `mod slot_table_generated` behind `#[cfg(target_os = "android")]` and dropped the
  `#![allow(dead_code)]` the module carried. Entries 24 and 25, and C37, C38 and C41 above, all recorded
  that Android clippy or clippy itself could not run on this machine. Both claims are wrong: the NDK is at
  `C:\Users\<user>\Documents\hachimi-global-recon\android-ndk-r27c`, whose
  `toolchains\llvm\prebuilt\windows-x86_64\bin` holds `clang.exe` (18.0.3), the
  `aarch64-linux-androidNN-clang` wrappers and `llvm-ar.exe`; `rustup target list --installed` lists
  `aarch64-linux-android`; `cargo clippy --version` is 0.1.98 against rustc 1.98.1, and clippy is in the
  stable toolchain's component manifests.

  The real blocker is host wiring, three details deep. (1) CI writes
  `$ANDROID_NDK_LATEST_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android24-clang` into
  `CC_aarch64_linux_android`. In a Windows NDK copy that extension-less name is a 209 byte bash wrapper
  (`"$bin_dir/clang.exe" --target=aarch64-linux-android24 "$@"`), `llvm-ar` likewise, and Windows cannot
  exec either, so cc-rs failed with `ToolExecError ... %1 is not a valid Win32 application. (os error 193)`
  inside `ring` and `blake3`. Those are third party build scripts, so `cargo clippy` stopped there and
  `hachimi` was never reached: that is what read as "no NDK clang". (2) `AR_aarch64_linux_android` needs the
  `.exe`. (3) `ring` preprocesses its `.S` asm through a clang temp file, so `TMP`/`TEMP`/`TMPDIR` must sit
  on a path the shell may write to; a temp dir outside the workspace gave `clang: error: unable to make
  temporary file: Permission denied` in `ring` with everything else wired correctly.

  Run on 2026-10-08 15:42 with `ANDROID_NDK_ROOT`, `CC/CXX_aarch64_linux_android` and
  `CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER` on `aarch64-linux-android24-clang(.cmd)` plus
  `AR_aarch64_linux_android=llvm-ar.exe` (and, equivalently, `clang.exe` with
  `CFLAGS_aarch64_linux_android=--target=aarch64-linux-android24`), CI's
  `RUSTFLAGS=-C link-args=-static-libstdc++ -C link-args=-lc++abi`, and a workspace temp dir:
  `cargo clippy --target aarch64-linux-android -- -D warnings` and the same with `--all-targets` both
  finished with **zero diagnostics**, under both tool spellings, after `ring` (24 objects plus its archive),
  `blake3` and `dobby-sys` (its prebuilt `dobby_static/android/arm64` archive) built for the target. So the
  android only surface of this series compiles and lints clean; nothing had to be changed in it.
  `cargo clippy --target aarch64-linux-android --all-targets -v` names the two units that were linted:
  `clippy-driver ... --crate-name hachimi src\lib.rs --crate-type cdylib --target aarch64-linux-android`
  and the `--test` unit, which is what compiles the `#[cfg(test)]` modules of the android only modules.
  Compiling them is not running them: `slot_table.rs`'s own tests (`failed_validation_is_cached_not_re_walked_per_lookup`,
  `retry_budget_is_bounded`) sat behind that cfg when this entry was first written: the cross target
  compiled them, the Windows `unit_tests` job could not compile them, and a cross target test binary cannot run
  on a Windows host, so nothing executed them. The 50 host tests recorded below were the count of that moment, and
  C43's cfg line has since moved those two into the host run. CI's Android step now runs clippy with `--all-targets` too
  (`.github/workflows/clippy_check.yml`), so that surface is linted on every push and not only when a
  human remembers to ask for it. The
  Windows leg (`cargo clippy --target x86_64-pc-windows-msvc --all-targets -- -D warnings`), `cargo check
  --all-targets`, `cargo build --release` (2 m 15 s) and `cargo test --lib` (50 passed, 0 failed at that
  moment; the tree this change set delivers measures 59, re-measured below) are clean.

  Proof that the leg reaches the gated code instead of passing by skipping it: a probe put into
  `src/il2cpp/slot_table.rs` (an unused private function, a `Vec::new()` followed by `resize`, and a
  `v.len() == 0` as the control) made the Android leg fail with `error: function leg_probe_unused is never
  used` at `slot_table.rs:68` and `error: slow zero-filling initialization` at `slot_table.rs:74`, noted
  `-D clippy::slow-vector-initialization implied by -D clippy::perf`, in both the `lib` and the `lib test`
  unit, while the Windows leg stayed clean over the same tree. That is the cfg gate working, the dropped
  `#![allow(dead_code)]` being enforced, and `perf = "deny"` live on the cross target; the control printed
  nothing, matching `all = "allow"`. The probe is gone and both legs are clean again.

  What this does not close: clippy emits metadata only, so nothing in the runs above linked. That gap was
  then filled with a claim that does not hold: `cargo build --target aarch64-linux-android` was recorded as
  failing at the link on this Windows host because `build.rs:69-70` and CI's `RUSTFLAGS` hand ELF options to
  lld (`lld: error: unknown argument: -z` five times, from `-z relro,-z,now` and `-z max-page-size=16384`,
  plus `--version-script`, `--no-undefined-version` and `--eh-frame-hdr`). Re-run 2026-10-08 16:57 in the
  delivered tree with the wiring above, it exited 0 after 25.25 s and wrote
  `target\aarch64-linux-android\debug\libhachimi.so`, 149,394,040 bytes, so build.rs's own
  `-Wl,-z,max-page-size=16384` and `-Wl,-z,common-page-size=16384` are accepted by the clang named as the
  target linker. Adding CI's `RUSTFLAGS=-C link-args=-static-libstdc++ -C link-args=-lc++abi` still exits 0,
  with one diagnostic, `linker stderr: clang: argument unused during compilation: '-static-libstdc++'`. No
  `-z` error appeared under either spelling here. What does fail is naming no linker at all: `CC/CXX` on
  `clang.exe` with `CFLAGS_aarch64_linux_android=--target=aarch64-linux-android24` and no
  `CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER` ends at `error: ``cc`` not found`, exit 101. The host linker
  gap sentence is withdrawn. `tools/android/build.sh` stays the device path because it also packages the
  module for a device, not because this host cannot produce an AArch64 shared object.
  Closing gate, and it is not met, so this item stays `[~]`. The run is what this item is about and it
  reproduces in the delivered tree, re-measured 2026-10-08 against `51941ea` plus this dirty working tree:
  `cargo clippy --target aarch64-linux-android --all-targets -- -D warnings` exited 0 with 0 diagnostics, the
  Windows `cargo clippy --all-targets -- -D warnings`, `cargo check --all-targets` and `cargo test --lib`
  exited 0, 0 warnings, 59 tests passed 0 failed, the 8 `il2cpp::slot_table::tests` included, which is
  C43's cfg change and supersedes the 50 recorded above. 58 was the count one round earlier; the delivered
  tree runs 59, `cargo test --lib -- --list` grouping them as 21 `AnimationSpeed`, 13 `Time`, 12
  `StoryTimelineController`, 8 `slot_table`, 5 `HighSpeedSetting`. The probe control was re-run here rather than
  remembered: a `#[cfg(target_os = "android")] fn leg_probe_unused` holding a `Vec::new()` followed by
  `resize` made the Android leg exit 101 with `function leg_probe_unused is never used` and `slow
  zero-filling initialization`, noted `-D clippy::slow-vector-initialization` implied by
  `-D clippy::perf`, in both its `lib` and its `lib test` unit, while the Windows `--all-targets` leg stayed
  at 0 diagnostics over the same tree. The probe is out of the tree and every leg is clean again, so the
  claim that this leg reaches the gated code instead of passing by skipping it is measured here, not
  asserted. Two things keep the item open: the commit hash section 10 wants now exists (`88837f7` carries
  the cfg gate, `4c8ff25` the generator, `a6f0adb` the two CI legs), and no Android game run exists, so
  nothing in this fork claims an Android hook was reached.

- [~] **C43 The android only cfg gate put the slot table reproductions where no test run could reach
  them.** The gate that keeps the 234 pinned offsets out of the Windows deliverable (the two
  `#[cfg(target_os = "android")]` lines over `slot_table` and `slot_table_generated` in
  `src/il2cpp/mod.rs`) also gates the module's
  `#[cfg(test)] mod tests`, so `cargo test --lib` here stopped compiling them: 50 host tests ran and not one
  of them was `il2cpp::slot_table::tests`. The two most recent slot table fixes lost their only executable
  coverage and no CI job had any either: the Android leg compiles `#[cfg(test)]` code with `--all-targets` but
  cannot run a cross target test binary, and the Windows `unit_tests` job cannot compile a module its target
  does not build.

  Fix, one line of cfg: both modules are gated `#[cfg(any(target_os = "android", test))]`. `check`, `clippy`
  without `--all-targets` and `build --release` on Windows never set `test`, so the deliverable and its lint
  scope are unchanged. A test build compiles the module, so its 8 tests, including
  `counts_alone_cannot_tell_a_mispinned_table_from_a_good_one` (the per name mismatch counter) and
  `failed_validation_is_cached_not_re_walked_per_lookup` (the never re-walk a rejected table regression), run in
  `cargo test --lib` here and in CI's `unit_tests` job on every push. Off Android the module stays inert:
  `find_libunity_base` under `cfg(not(unix))` returns None, so the probe reads no address and every lookup
  answers 0 (`resolve_is_inert_without_libunity`).

  Controls that show the gate moved only where intended, with a probe in `slot_table.rs` (an unused private
  function, a `Vec::new()` plus `resize`, the C42 shape): `cargo check` and `cargo clippy -- -D warnings` on
  Windows finished clean with the probe in place, so a non-test Windows build still does not compile the
  module; the same tree made `cargo test --lib` report `function leg_probe_unused is never used` against
  `hachimi (lib test)`, made `cargo clippy --all-targets -- -D warnings` fail with that error plus
  `-D clippy::slow-vector-initialization` implied by `-D clippy::perf`, and made
  `cargo clippy --target aarch64-linux-android --all-targets -- -D warnings` fail in both its `lib` and
  `lib test` units, so the Android half of the gate is untouched. The probe is gone and every leg is clean
  again. AGENTS section 4 now describes the gate as it is.

  What this does not verify: the host run reaches `validate()` through the `cfg(not(unix))` stub, which answers
  "libunity.so is not loaded", so the host tests prove the cached verdict and the bounded budget (no full probe
  after a cached failure, at most `MAX_VALIDATE_ATTEMPTS` per process) and the pure tally arithmetic, not a
  rejection by a fingerprint or a sampled name against a real libunity, and never a `dl_iterate_phdr` walk.
  Whether a device adopts the table, what `diag[post-load]` reports there and what the bounded retries cost is
  a device run, and no Android run exists.

  Closing gate, and it is not met either, so this item stays `[~]`. The cfg line reproduces in the delivered
  tree, re-measured 2026-10-08 against `51941ea` plus this dirty working tree: `cargo test --lib` ran 59
  tests, 0 failed, and 8 of them were `il2cpp::slot_table::tests`, the two named above included. `cargo
  check --all-targets`, `cargo clippy --all-targets -- -D warnings` and `cargo clippy --target
  aarch64-linux-android --all-targets -- -D warnings` all exited 0, the last one with the 234 offsets and
  the test unit compiled for the target, and the C42 probe control, a `leg_probe_unused` inside a
  `#[cfg(target_os = "android")]` block, made that leg exit 101 while the Windows `--all-targets` leg stayed
  clean over the same tree, so the gate moved only where intended. What section 10 wants on a closed item,
  the commit hash carrying the cfg line, does not exist: the change set is left uncommitted by instruction,
  and a device run is still the only answer to whether a device adopts the table.

### Ledger gate check for this change set, measured against `51941ea` and the dirty tree

Two numbers this change set recorded about itself do not hold on the tree it delivered, and the second is
not what the command it names produces there. What was recorded: no `[x]` awarded anywhere in the change
set, count 0, and the ledger's remaining `[x]` items are 7 pre-existing run-backed ones. What the delivered
tree measured, before this block was written:

- `git diff -- DEFECTS.md | Select-String '^\+- \[x\]'` -> 2 added items, not 0. They were `- [x] **C42`
  and `- [x] **C43`, and a third added line carried the word `[x]` in prose. `git show HEAD:DEFECTS.md` has
  0 matches for `C42` or `C43`, so both closures are new in this change set.
- `Select-String '^- \[x\]'` -> 0 over `git diff` and 9 over the file. No reading of this file or of this
  diff produces 7.
- Top level `- [x]` items: 8 at HEAD, 9 in the tree. The two extra were C42 and C43; one HEAD item,
  `scale_frame_count` put to use by `SetHighSpeedFrameCount`, had been demoted to `[~]` in the same diff.
  Nested `- [x]` items: 1 at HEAD and 1 in the tree. Numbered `[x]` items in section D: 5 at HEAD and 5 in
  the tree. None of those were added or removed.
- Item counts: 15 top level items added (13 `[~]`, 2 `[x]`), 5 removed (4 `[ ]` promoted to `[~]`, 1 `[x]`
  demoted), 7 numbered `[~]` items added in D. The change set is 28 modified files, 0 staged, 0 untracked;
  line totals move every time the ledger is edited, so only the marks are quoted here.
- The round's own DEFECTS.md citations do not point at what it delivered either: the lines it named, 757,
  762, 766 to 773 and 768, sit inside the C42 entry, and the entry it actually edited is the one headed
  "E3 This fork's own Performance labels were English-only in nine locales" in section E. A ledger that is
  still being edited moves its own line numbers, so cite an entry by ID and heading, not by line.

The commands as written could not have measured what they claimed. Git prefixes every diff line with `+`,
`-` or a space, so an added top level ledger item prints as `+- [x] **C42 ...` and a removed one as
`-- [x] ...`, and a pattern anchored on `^- \[x\]` matches neither shape. A check that cannot match what it
looks for returns a clean number on any tree, which is how an unclean gate gets reported as passing. The
shapes that do measure a ledger are `^\+- \[x\]` for a closure awarded by the change set, `^-- \[x\]` for a
closure taken away, `^\+- \[~\]` for an item opened or demoted, and `^- \[x\]` only against the file, never
against a diff.

Why the gate matters here: `[x]` is this fork's terminal mark, section 10 backs it with a commit hash and
"never mark `[x]` without a run", and the round written to take an `[x]` away from an item that had no run
shipped two new `[x]` closures. C42 and C43 now sit at `[~]`, each naming the runs that do back it and the
gate it still lacks. Measured again after that correction: `^\+- \[x\]` -> 0, `^-- \[x\]` -> 1, and the
ledger reads 7 `[x]`, 22 `[~]`, 36 `[ ]` and 3 `[latent]` at top level, C items C1 to C43, which is why the
section heading above read 34 and now reads 43. The tree carries one top level closure fewer than the tree
this change set started from: it awarded none and retired one. `C7` and `C27` moved from `[ ]` to `[~]` in
the same pass, each naming the part repaired and the part left open.
The correction is not only the two marks. C42's body also claimed that no job here or in CI executed
`slot_table.rs`'s two tests, which was true while that module sat behind `#[cfg(target_os = "android")]`
alone and is false in the delivered tree, where `cargo test --lib` runs 8 `il2cpp::slot_table::tests`; the
entry now says which count belongs to which moment, and AGENTS section 10 states the mark rule plus the
diff shapes a ledger self check has to match.

Counts quoted inside single entries (45, 48, 50) are snapshots of the moment that entry was written, not of
the delivered tree. The delivered tree was measured once, against `4e40dbf`: `cargo test --lib` ran 59 tests
with 0 failed, `cargo check --all-targets` and `cargo clippy --target x86_64-pc-windows-msvc --all-targets
-- -D warnings` exited 0 with no diagnostics, `cargo clippy --target aarch64-linux-android --all-targets
-- -D warnings` exited 0 with the 234 offsets and the test unit compiled for the target, and `cargo build
--release` finished in 2 m 30 s with no warnings. That closes the two problems the fix series ended on: the
android link claim C42 repeated is withdrawn above, and 59 is the count every entry now quotes for this tree.

- [~] **C45 The Config Editor and the setup wizard offered `ui_animation_scale` up to 1000.0 while the
  code honours 20.0.** `normalize_ui_animation_scale` (`AnimationSpeed.rs:866`) clamps the lever to
  `MIN_UI_ANIMATION_SCALE..=MAX_UI_ANIMATION_SCALE`, and `MAX_UI_ANIMATION_SCALE = MAX_FACTOR = 20.0`,
  before the DOTween `Update` detour multiplies its delta time. The top of the slider therefore did
  nothing, and the live config carried 1000.0 while the game ran at 20.0. `baa5570` puts both sliders on
  the constants, the Performance tab and the first time setup wizard, so the offered range is the honoured
  range. `[~]` until a launch shows the bounded slider. Behaviour does not change: the stored 1000.0 was
  already clamped at read.
- [ ] **C46 `story_text_speed_multiplier` has no ceiling in code at all.**
  `StoryTimelineData.rs:106-110` reads `get_TypewriteCountPerSecond`, multiplies it by the raw config
  value and writes it back, unclamped, on every story timeline asset load. The slider offers 0.1..=1000.0
  and this client's config carries 1000.0, so the typewriter runs at 1000 times the game's rate while every
  speed lever this fork added is capped at MAX_FACTOR (AGENTS section 2). Unmeasured: whether one asset
  object can reach that path twice and compound, which is the C22 shape, and what the client does with a
  typewriter rate that large. It is an upstream option, so bounding it is a behaviour decision. Decision taken 2026-10-08: leave the
  slider and the value as they are, story text stays at 1000 times the game's rate, and C46 stands as a
  measurement note rather than a fix. `story_speed` was also left at 1.0..=10.0 even though the code would
  honour MAX_FACTOR, because 10 is the value the runs measured with.

- [~] **C47 The probe chose a cut run boundary that this client never calls.** Run 9 installed 18 of 19 probes and
  printed `cut runs 0` on all 22 totals lines across 617 s of training, while `CutInTimelineController::UpdateSpeed`
  reached 3194 calls and `SkipRuntime(time)` 70, so the timeline was plainly running. `ResetCurrentTime` is in
  `Gallop.CutIn.Cutt.CutInTimelineController` (dump L26098) and was hooked, but a training cut-in does not go through
  it, and neither do `SetSpeed`, `get_CurrentTime`, `get_CurrentTimeScale`, `get_WaitingTime`, `SkipRuntimeFrames` or
  `SkipTimeDirect`. Evidence: `hachimi.log:550` and the totals lines L801 to L999. A boundary chosen from a class name
  is a guess, and this is what a guess costs. The replacement named here, `set_IsPlayingCutt/1 -> void(bool)` on
  `SingleModeMainTrainingCuttController`, was installed in `19e741c` and run 10 showed it is silent too (C49). The
  boundary is still not found, so this stays open.
  - Done: the replacement boundary installed. Not done: a boundary that the client actually calls.

- [x] **C48 A generic collection parameter does not match `CLASS`, so one probe never installed.**
  `TrainingParamChangeUI::InitializePlateList` was the only probe of 19 that failed (`hachimi.log:551`, reported as
  "no class or no matching overload"). The dump gives it as
  `InitializePlateList/2 -> void(generic<System.Collections.Generic.List<Gallop.TrainingParamChangeUI.ChangeParameterInfo>:24B>, float)`
  (dump L24571), and the probe asked for `IL2CPP_TYPE_CLASS` for that argument. A generic instantiation carries
  `Il2CppTypeEnum_IL2CPP_TYPE_GENERICINST` (21) in its parameter record, not `CLASS`, so the signature walk rejected a
  method that exists. The resolver's note that "`CLASS` matches every reference type" (AGENTS section 5) is true for
  plain reference types and false for a generic instantiated one. Fix: let the probe's parameter matcher accept
  `GENERICINST` alongside `CLASS` for a reference slot, which is safe for an observe only hook because the probe never
  reads or writes the argument, it only matches the overload.
  - Fixed in `0e98898` and closed by run 10: the log reads `InitializePlateList parameter 0 is a generic
    instantiation, bound as the pointer this wrapper declares` followed by
    `new_hook!: TrainingParamChangeUI_InitializePlateList`, and the door reached 102 calls with an interval peak of
    1.000. The exact overload walk still runs first, and only `resolve_generic_ref_method` and
    `resolve_static_generic_ref_method` ask for the widening, so no scaling hook can bind a generic parameter.

- [ ] **C49 The second boundary guess is also a door this client does not use.** Run 10 measured a real career and
  still printed `cut runs 0`, while the same totals line showed 19 `PlayTrainingCut(info)`, 19
  `PlayTrainingCutEndAsync(id, flag, flag)`, 35 `CleanUpCutt()`, 38 `TrainingCutStatus::PlayIn(duration, count, flag,
  action)` with the duration 5.640, 18 tag `PlayCutIn(cards, done)`, 2274 `CutInTimelineController::UpdateSpeed()` and
  19 `WaitTapAsync()`. `set_IsPlayingCutt(playing)` and `get_IsPlayingCutt()` printed nothing, and 29 of the 47
  counted doors printed nothing at all, including the three `Update/FixedUpdate/LateUpdateTrainingCutIn` drivers,
  `TrainingAsync`, `PlayScenarioTrainingCut`, `PlayTrainingSaboriAsync`, `IsValidTag` on both classes, `set_SkipFrame`,
  `set_IsAutoPlay`, `get_IsAutoPlay`, `IsAutoPlay`, `SetSpeed`, `SkipTimeDirect`, `SkipRuntime(frames, keep)`,
  `get_CurrentTime`, `get_CurrentTimeScale`, `get_WaitingTime`, `ResetCurrentTime`, `GetTotalFrameCeil`,
  `get_CurrentFrame`, `WaitTap`, `CoroutineDoTweenTimeScale`, `ExistPlayingFrame`, `GetIntervalOutBegine` and
  `WillRankUpInHighSpeedMode`. Evidence: the `Cutt probe totals at 458 s` line of the run 10 log. The pairing the data
  supports is `PlayTrainingCut` open, `CleanUpCutt` close, and a run attributed to whichever door opened it.
  - Not done: the pairing. Nothing is scaled through a run yet, so no behaviour is exposed to this.

- [ ] **C50 The story cut-in engine runs all career and its entry door is not `CutInHelper::Play`.** Run 10's story
  probe printed `cut-ins 0` while `CutInHelper::GetTotalTime()` was called 757 times with a peak total time of 5.667,
  `ContextExtension::FixedUpdateForHighSpeed(helpers)` 4769 times, `SetTimeAll(helpers, time)` 1512 times peaking
  5.667, `SkipPause(helpers)` 756, `CutInHelper::CleanupPlaying()` 36 and `CutInHelper::IsPlaying()` 54 with a peak of
  1.000, so a cut-in was playing. `Play(timeline, parent)` and `OnPlayCutIn(timeline, parent)` were silent, as were
  `CutInHelper::GetTargetSpeed`, `CutInHelper::FixedUpdateForHighSpeed(rate)`, `CutInHelper::SetCurrentFrame` and
  `ContextExtension::GetCurrentTime`. Evidence: the `Story event probe totals at 441 s` line.
  - Cause found in the same run's dump. `Gallop.SingleModeTrainingCutInHelper` does not call the two argument `Play`
    on its base; it declares its own set, and those are the doors the probe never asked for: `Play/0 -> void()` (dump
    L26598), `OnPlayCutIn/0 -> void()` (L26599), `OnStartCutIn/0 -> bool()` (L26600), `OnPlayMainCutIn/0 -> void()`
    (L26602), `OnEndCutIn/0 -> void()` (L26603), `CleanupPlaying/0` (L26604), `OnTerminateRuntime/1 ->
    void(CutInTimelineController)` (L26595), `OnTimelineUpdatePost/1 -> void(float)` (L26607), `SnapShot/0` (L26593)
    and `SetDrawSnapShot/0` (L26594). The base `Gallop.CutInHelper` carries the lifecycle and the pause doors the fork
    has never looked at either: `OnStart/0` (L26462), `OnEnd/0` (L26463), `OnEndCutIn/0` (L26438), `StopRequest/0`
    (L26428), `Pause/2 -> void(bool, bool)` (L26430), `IsPause/0 -> bool()` (L26431), `OnRestart/0` (L26434) and its
    state property `get_Status/0` with `set_Status/1 -> void(struct<Gallop.CutInHelper.CutInStatus:4B>)` (L26409,
    L26410), a 4 byte enum of the kind A5 confirms travels safely in a general register.
  - Not done: the pairing. `Pause/2` with `IsPause/0` and `StopRequest/0` are also the game's own pause and stop doors
    for a cut-in, which is what item 41 has been looking for on this screen.

- [ ] **C51 Every training scaling door this fork installs was never called.** Run 10 printed
  `TrainingFooter.GetItemAnimDuration=0 TrainingFooter.GetCloseAnimWaitTime=0 TrainingCuttClip.get_DelayTime=0
  SingleModeUtils.GetHighSpeedPlayDuration=0 SingleModeUtils.GetCutTimeScale=0`, which is the third consecutive run
  where the five `TRAINING_HIT_SLOTS` doors (`AnimationSpeed.rs:904` to `:910`) printed zero. What the training screen
  actually runs through is `SingleModeTrainingCutInHelper::GetTargetSpeed` 598 calls peaking 11.280,
  `SingleModeUtils::GetTrainingCutTimeScale` 76 calls peaking 11.280 (double its argument, matching run 9), and
  `CutInTimelineController::UpdateSpeed` 2274 calls, none of which the mod scales. Any future training speed option
  has to sit on those doors, and the rule against scaling only a getter applies to them: `GetTargetSpeed` has a
  matching `set_` path on the same timeline, so a getter only change would compound (C22, C24).

## D. Fix order

1. [x] A1 duration argument index in `NowLoading` (`db3c272`).
2. A2 overload matching by parameter class name.
3. A3 argument-only wrappers for static targets, then re-enable both high speed helpers.
4. A5 hook `PlayFadeFrontCanvas` and the `HighSpeedType` enum overload.
5. C2 unwind and SEH barrier at hook entry, `unwrap_or_else` on shared mutexes, bypass instead of
   `process::exit`.
6. [~] C5 clamp `ui_animation_scale` in code (`AnimationSpeed` mirror, 0.1..=MAX_FACTOR) and
   leave `independent_time` alone: the clamp landed, both sliders now sit on the constants too (C45,
        `baa5570`), and the `independent_time` half is still open.
7. [~] C22 and C24, the two compounding multipliers that are live in the current config. C24's two
   sites now read one clamped mirror and each half is capped by the quantity it touches (MAX_FACTOR on
   the wait time increment, MAX_TIME_SCALE on the story time scale); the same factor is still applied
   on both paths, which is the item's root cause, and C22 is untouched.
8. C1 remaining `get_orig_fn!` audit and the proxy export stubs.
9. A6 `Show/7` and `Hide/3` still open. A7 is `[~]`: the reference bit is verified before a wrapper
   writes back and one method is hooked through it (`be41b21`), the matcher itself is unchanged.
10. [x] `src/il2cpp/hook/umamusume/HighSpeedSetting.rs` raises the game's own settings: it reads
    `StoryManager::GetMaxHighSpeedType/0`, writes story through `StoryManager::SaveHighSpeedType/1`
    and training through `ApplicationSettingSaveLoader::set_TrainingHighSpeedType/1`, gated on
    `high_speed_settings` (default false). Remaining on this item:
    - [ ] the numeric members of `Gallop.StoryTimelineController.HighSpeedType` are unknown because
      the enum class is not in the dump, so the `HighSpeedSetting: max ...` log line is the only
      source of truth for the first run
    - [ ] `StoryManager::ChangeAutoHighSpeedSettingAndSave/1` and `ChangeSavedAutoHighSpeedSetting/0`
      are not called; they may cycle rather than set, so they need a probe before use
    - [ ] `StoryManager::get_IsHighSpeedMode/0 -> static bool` is not hooked
    - [ ] `StoryTimelineController::SetHighSpeedFrameCount/2 -> void(class<StoryTimelineTextTrackData>, int)`
      and `/1 -> void(class<StoryTimelineTextClipData>)` are not hooked; only the `(int)` overload is
    - [x] `high_speed_settings` is in the Config Editor, on the Performance tab (`e705585`, `3adb2bf`)

11. Read the next run before choosing which single link on the story frame count path is safe to
    scale: the line `Hooking finished: N hooks armed in one pass, S s` against the measured 4.68 s,
    the `Story step StoryTimelineController::SetHighSpeedFrameCount call N: value` lines, the
    `Story step StoryTimelineController::GetNextFrameCount_HighSpeed call N` counters and the six
    `GetNextFrameCount_HighSpeed frames a, count b (story xN, both passed through)` value lines, and
    whether the 428.6 s of screen time in run 2 moved. A scale on one link only is the next step;
    which link, and against what bound, is what those lines have to say.
12. The story text path is only reachable through the methods that read its constants.
    `StoryTimelineTextClipData.TYPEWRITER_WAIT_FRAME`, `StoryTimelineController.FADE_TIME_FOR_HIGH_SPEED`,
    `TOUCH_BLOCK_INTERVAL` and `CONTINUOUS_TOUCH_INTERVAL` all logged as compile-time constants in run 2,
    and `resolved 0/61 duration fields` still holds. A13 is the first of those methods.
13. [x] The frame stepping path this client uses is now measured. It was unknown because
    `GetNextFrameCount_HighSpeed`
    and `SetHighSpeedFrameCount` are installed, signature verified, and never called in a 391 s
    career run that reached training turns, races and five result screens. [StoryFrameProbe.rs](src/il2cpp/hook/umamusume/StoryFrameProbe.rs)
    observes fifteen candidates with every argument handed to the original untouched:
    `SkipFrameCount`, `SetFrameCountForWaiting`, `SkipMotionFrame`, `IsSkipToTextClip`,
    `get_WaitFrameCountUntilNextBlock` and its setter, `get_WaitFrameUntilNextBlock` and its setter,
    `get_WaitingFrameCount`, `UpdateTimeScaleByHispeedType`, `get_TimeScale` and `set_TimeScale`,
    `IsHighSpeedMode`, `IsStoryEndFrameOrGrandLiveWaitFrameSkipped` and
    `StoryTimelineTextClipData::GetWaitFrameUntilNextBlockLocalize`. The read half and the write half
    are both probed because a busy getter measures polling while a busy setter measures advancement,
    and the `TimeScale` pair is who writes the value C22 compounds. Nothing is matched on name plus
    arity: every candidate goes through `AnimationSpeed::resolve_method` or the new
    `resolve_static_method`, both of which check the dumped parameter types, the return type,
    genericness, the static flag and whether each parameter is a reference. It installs only under
    debug_mode, logs the first six calls of each probe, prints `Frame probe totals at N s:` every
    20 s while the totals grow, and lists any candidate that never resolved at install time. A probe
    that never appears is itself the answer, and A13 can only be pointed in a direction once those
    totals exist. Run 4 below supplies them.
14. [x] The stepping answer from the run 4 totals. `SkipFrameCount(int, bool, bool)` and
    `SkipMotionFrame(int)` are the paths this client actually uses for story and career text, 579 and
    714 calls, receiving the same frame values, and `UpdateTimeScaleByHispeedType` ran 211 times. The
    wait frame properties are read 404 times and never written: both setters are 0,
    `get_WaitFrameUntilNextBlock` and `get_WaitingFrameCount` are 0. The static `get_TimeScale` and
    `set_TimeScale` pair is 0 even though `UpdateTimeScaleByHispeedType` ran, so the story scale lives
    in instance state and not in that static property. `IsHighSpeedMode` was called 34635 times and
    returned 0 in every logged call, `IsSkipToTextClip` and
    `StoryTimelineTextClipData::GetWaitFrameUntilNextBlockLocalize` were never called at all. The 512
    chunk cadence is too fine for a predicate called about 110 times a second and produced 66 of the
    1026 lines, so hot probes want a 4096 cadence.
15. Two candidate levers follow from item 14, neither attempted yet.
    - `StoryTimelineController::SetHighSpeedType/1 -> static void(struct HighSpeedType:4B)` is the
      game's own switch for its fast story path. `IsHighSpeedMode` returning 0 through a whole career
      session says that path is not engaged even though the saved high speed setting is 2, and the two
      counters that gate on it are the busiest thing in the log. Engaging it is a game owned setting
      change in the same shape as `HighSpeedSetting`, and a 4 byte struct parameter is confirmed to
      travel in a general register (A5).
    - Scaling the `SkipFrameCount` and `SkipMotionFrame` frame arguments is the direct lever. Both are
      single int arguments rather than read-modify-write state, but C23 (story block length
      recomputation) has to be settled before multiplying a skip request.
16. [~] `GetNextFrameCount_HighSpeed` is still not proven uncalled, and the ledger should not paper over
    that. Its detour used to log only when scaling changed the value, so a call whose scaled value came
    out identical would be invisible, and the probe set had no plain counter for it or for
    `SetHighSpeedFrameCount`. Both paths now carry a plain counter that logs `Story step <name> call N`
    for its first six calls and then every 4096, and both log the value the game actually produced:
    `SetHighSpeedFrameCount call N: value` for the setter, and `frames a, count b` for the reference
    pair. Run 5 produced twelve `Story step` lines and all twelve were the two skip paths, so neither
    counter fired; that is evidence, not proof, because the story in that run stalled. With the
    scalings off these two paths (A13, C34) the counters and the value lines are the whole hook.
17. [~] Story high speed mode is a lever that has never written anything. Run 4 showed that raising the
    High Speed setting does not engage the mode the timeline consults, so `story_high_speed_mode`
    (default off) calls the game's own `StoryTimelineController::SetHighSpeedType` with whatever value
    `StoryManager::GetMaxHighSpeedType` reports. Nothing is written unless the game's own
    `IsHighSpeedMode(value)` predicate accepts that value first, so no enum value is invented, and the
    write only happens while the skip paths were reached within the last 10 s, at most once per 10 s,
    from the game thread, and only for a story state this module has not already written for
    (item 24). Run 5 had the option on, all three helper addresses resolved, and produced no
    attempt line and no rejection line, while its probe still sampled `IsHighSpeedMode()` returning 0.
    Three exit paths were silent; each now names its reason, and the config mirror logs the state it
    adopted (`ac2b1a5`). The lever needs one clean run with nothing else touching story timing before
    it is trusted.
18. [x] Skip frame extension was tried in run 5 and removed (`ac2b1a5`, `a2c5f67`, `30e1cb4`,
    `8505cfa`). At any value above 1.0 it stalled story advancement: the extension landed on the ceiling
    on every call, and the game forwarded the raised value from `SkipFrameCount` into `SkipMotionFrame`,
    which raised it a second time. See C34 and the run 5 block. `SkipFrameCount` and `SkipMotionFrame`
    stay hooked as counters that log the frame value they carry and pass every argument through
    untouched.
19. The probe set is 13 candidates in the build that produced run 6: `SkipFrameCount` and
    `SkipMotionFrame` moved from observation to real hooks, and the summary cadence went from 512 to
    4096 after `IsHighSpeedMode` wrote 66 of the 1026 lines in run 4 at the old cadence. Run 6 shows
    the cadence is still too tight for `IsHighSpeedMode`, which polled 19737 times in 148 s.
20. [x] The `IsHighSpeedMode` probe is gone and the set is 12 (`1bef16b`, run 7 prints
    `story high speed mode 1 writes 4` on every totals line). A hook on a path the game polls 133 times a second makes the mod pay a
    `get_orig_fn!` map lookup on every poll, which is our cost, not the game's. The mode is now read
    once per report through the guarded wrapper and printed as `story high speed mode 0/1` on the
    totals line, which answers the same question for a fraction of the calls and of the log.
21. [x] A3 is closed for the two story time scale helpers (`781e3e1`, run 7 answered both questions, see
    A17):
    `GetTimeScaleByHighSpeedType/1` and `GetTimeScaleHighSpeed/1` are static and take a bool, so they
    are now resolved with `resolve_static_method` behind argument only wrappers, and they are
    installed as measurement. Each logs `Story scale <name>(flag) call N -> value` for its first six
    calls and then every 4096. Nothing is scaled yet. The next run answers the two questions that have
    to be answered first: whether the game calls them at all (they may be inlined into
    `UpdateTimeScaleByHispeedType`, which is hooked and ran 109 times), and what the game returns for
    each flag. Putting a factor on them without those numbers is run 5 all over again.
22. [ ] The auto high speed subsystem is the better door into the mode than the timeline static. The
    dump lists the game's own path (`StoryManager::ChangeAutoHighSpeedSettingAndSave`,
    `StoryViewController::ApplyAutoHighSpeedSettings`, `StorySceneController::
    ApplyAutoHighSpeedSettingsToModel`, and `StoryManager::get_IsHighSpeedMode` as the state the game
    itself believes). Driving the mode through the feature the game already implements means the
    game also runs its own fade and audio handling for high speed, which a raw `SetHighSpeedType`
    write skips.
23. [~] Neither game owned write had a restore path: `HighSpeedSetting::apply` returned early the
    moment `high_speed_settings` went false, so StoryManager's saved setting and the save loader's
    training value stayed at the number the mod wrote and the game went on saving them, and the
    static `StoryTimelineController::SetHighSpeedType` set stayed set because `apply_config` only
    mirrored the option. Both now remember what the game had before the first write and put it back
    once when the option is turned off, gated on a `WAS_RAISED` / `HIGH_SPEED_WAS_SET` marker in the
    shape `WAS_SCALED` uses. The settings read their baseline from the game's own getters,
    re-observed whenever the current value differs from what the mod last wrote. The static has no
    value getter, so its baseline is recorded by a pass-through detour on `SetHighSpeedType`,
    installed only while the option is on (the StoryFrameProbe rule: at the neutral default this
    module adds no detour to a game path), with our own writes excluded by a guard flag the way
    Time.rs's `APPLYING` does. With no game write on record the only value offered is one this
    client's own `IsHighSpeedMode(0)` reads as no high speed mode, because the engage path only ever
    wrote when the game reported the mode as off; when even 0 reads as high speed, nothing is
    written. Unit tests cover the baseline choice and the once-only pass; no run has yet. The next
    run must show `story high speed N -> M restored ...` and `restored story high speed type M`, and
    whether the recorder resolved (`story high speed helpers setter 1, value predicate 1, state
    reader 1, baseline recorder 1`).

24. [~] `engage_high_speed_mode` had no applied marker, so it re-applied the mode switch on its
    timer. The pass read `IsHighSpeedMode()` and wrote `SetHighSpeedType` whenever that read
    answered off, so a scene whose blocks or clips clear the mode paid the write once per 10 s gate
    inside one scene, and every one of those writes is a call into the game's own high speed mode
    switch - `FADE_TIME_FOR_HIGH_SPEED`, `SetBgmClipEnabledOnSwitchHighSpeedMode`,
    `ForceStopBgmOnHighSpeed`. A10's shape again: an idempotency test asking the wrong question.
    The gate is now an applied marker. The pass through detour on `SetHighSpeedType` ticks a state
    version per write the game made itself, engage remembers the value and the version it wrote, and
    a write is due only when the game moved that state (`already_written_for_state`, the shape
    Time.rs's `APPLIED_LEVER` and `AnimationSpeed`'s `APPLIED_FACTORS` use). The mode read stays
    as the guard against overwriting a mode the game engaged on its own, and the 10 s interval stays
    a rate cap. `HIGH_SPEED_WRITES` counts every value this module put on the static: each write
    line reads `write N`, the already written exit prints the running total, and the probe totals
    line ends `story high speed mode 0/1 writes N`. The restore pass clears the marker so a later
    scene can ask again, engage records nothing when the setter did not resolve, and turning the
    option on with no recorder armed says so on the option line. 45 unit tests pass and clippy is
    clean for the Windows target and for `aarch64-linux-android` (C42) and nothing added
    is platform specific. Two questions keep this `[~]` until a run answers them: with no recorder
    armed the state version never moves, so the static is asked for once per session, and if the
    game clears the mode through a store the recorder does not watch the fix chooses one write over
    re-asserting, which is why item 22's game owned auto high speed path is still the better door.
    The line to read is `asked the story timeline ... (write 1, story state N)` with no further
    write line for the rest of that scene.

25. [~] The hardened matcher never proved that a value type actually travels by value.
    `resolve_method_any` checked the name, the arity, the parameter enums, genericness, the static
    flag, the reference bit and the return enum, but never the class behind a `VALUETYPE`, so
    `struct<SomeStruct:24B>` and `struct<Gallop.StoryTimelineController.HighSpeedType:4B>` were the
    same answer to a wrapper declaring an `i32` (A5 says only the 4 byte one travels in a general
    register). `StoryTimelineController`'s two enum candidate loops compounded it: `CLASS` was a
    candidate, so a client spelling the parameter as a reference would have bound it behind
    `SetStoryHighSpeedType`/`IsHighSpeedModeValue` and received the enum value in the register the
    game expects an object pointer in, and the only install line printed three booleans that could
    not tell a `class` install from a `struct` one. `value_shape_for` and `shape_is_allowed` measure
    the payload now (`il2cpp_class_instance_size` minus the 16 byte object header, the same
    subtraction `introspect.rs:135` prints) and refuse anything over `MAX_INLINE_VALUE_BYTES = 4`,
    every reference sitting in front of a value-shaped wrapper, and any type the matcher cannot
    classify; the shape it accepted and the one it refused are both printed with the concrete type
    name and size. The new `resolve_static_value_method` carries that rule for the two enum helpers,
    and the install line names the candidate each one bound to, so the line item 24 asks a run to
    read is now `story high speed helpers setter 1 (struct), value predicate 1 (struct), state
    reader 1, baseline recorder 1`. The three StoryManager statics in `HighSpeedSetting` came off
    `get_method_addr` (C7) onto `resolve_static_method`, the matcher written for a static with no
    `this`, with the value proof applied to their `struct<HighSpeedType:4B>` results and parameter;
    their line names the spellings too (`HighSpeedSetting: StoryManager statics max struct, saved
    struct, setter struct`). The decision functions are unit tested (48 pass, the three new ones
    cover the 4 byte enum, an 8 and a 24 byte struct, an unreadable class, a `class` parameter and
    a `generic` one) and clippy is clean for the Windows target and for
    `aarch64-linux-android` (C42), and nothing added is platform specific. A run has to confirm the three proof
    lines land (`... travels by value: Gallop.StoryTimelineController.HighSpeedType:4B`), that all
    five addresses still resolve, and that no existing hook that takes a `class<...>` argument went
    inert - `resolve_getter`, the fades and the frame probes keep the permissive match rule.

26. [x] The story time scale lever is back on the path that measured it (C39): the three Story getter
    hooks scale through `scale_read_time_scale`, which raises the game's 1.0 and leaves a stored pause or
    slow motion as the game stored it. The line a run has to read is
    `AnimationSpeed: StoryTimeline_getTimeScaleAfterEndStory 1 -> 5`, or the `getTimeScaleEventWipe` or
    `GetCutTimeScale` spelling, with `story_speed` above 1.0 on the `Config snapshot:` line, plus the
    `Time::set_timeScale call` lines for what the game finally holds, the mod's own raise capped at 5
    and a scale the game already holds above 5 left where the game put it. Until that line exists
    no screen time claim is made for this lever.

27. [x] Read the apply pass totals the next run produces (C41, run 7 printed them): `AnimationSpeed apply pass N: a config
    reads, b entry locks, c table passes, d field reads, e field writes` is what C36's cost claim rests
    on now, printed for the first 6 passes and then every 64th. Expect three config reads per pass that
    ran and one entry lock per pass that found a group due; on this client `d` and `e` stay 0 while 0 of
    61 duration fields resolve (C13). `cargo test --lib` runs in CI on a Windows job from this change
    on, so the shipped decisions are checked on every push; no number in this ledger may quote a per
    field cost without one of these lines in front of it.

28. [x] The two BOOLEAN story predicates were read through `i32` wrappers (`75f2147`). Run 7 read both halves correctly: `IsHighSpeedMode(2) 1`, four `IsHighSpeedMode 0 -> 1` writes matching `story high speed mode 1 writes 4`, and one attempt line that wrote nothing for a mode the game already had on. IL2CPP delivers a
    `bool` result in `AL` alone, so `before` and `accepted` in `engage_high_speed_mode` branched on the 24
    bits above it, and that branch is the gate deciding whether this module writes
    `StoryTimelineController::SetHighSpeedType` at all: a mode the game reported off could read as already
    on, and a value the predicate refused could read as accepted. `IsStoryHighSpeedMode` and
    `IsHighSpeedModeValue` now declare `bool`, which is the shape the matcher already proved of those
    addresses, `resolve_static_method` only handing back a candidate whose `il2cpp_method_get_return_type`
    is `IL2CPP_TYPE_BOOLEAN`, and `StoryFrameProbe` reads the same method through the same shape for its
    totals line. This is the one ABI claim here that was reproduced instead of argued: one address called
    through both signatures under MSVC x64 `/O2`, a genuine `false` reading back as `0x000aae00` through
    four bytes. A run has to show `story high speed mode 0/1 writes N` and no write line for a mode the game
    left on.

29. [~] `SetHighSpeedFrameCount` and `GetNextFrameCount_HighSpeed` are measurement only now (`75f2147`), the
    same decision C34 forced on `SkipFrameCount` and `SkipMotionFrame`. The number being scaled is an index
    into the readonly `_highSpeedFrameCountArray` on one reading and a count on the other, `scale_frame_count`
    floors at 1 so a scaled index can select a different, longer entry, and run 5 showed the game feeding one
    hooked step's output into the next. Both keep their counters and their value logs, so the next run says
    what the chain carries before anything on it is scaled again.

30. [~] Labels, log lines and scratch state in the delta (`3b24a75`, `922e2ff`, `4e40dbf`, `51941ea`):
    `ko.yml` is a Korean file again, the `ows:` section no reader asks for is gone and `hachimi:` and
    `windows:` are back; the six Performance labels this fork added sit once in each of the ten locales; the
    `Config snapshot:` line names `target_fps_unfocused` on Windows and `cyspring_mono_uncap_frame_scale`;
    the `time_scale` and `story_choice_auto_select_delay` sliders offer no range the code would clamp;
    `cargo_check_out.txt` is out of the tree and ignored. Verified by parsing the ten files, counting each key
    once, and the host legs named above. A run still has to show the Config Editor drawing those six labels in
    the language `locale` selects (E3), and C27 records that the unique wrapper name only covers this fork's
    own new hook.

31. [ ] Measure the launch and title bucket before touching anything else in it (run 8: Title 127.8 s of a
    362 s career, launch to first training screen 140 s, our own hook arming 0.059 s of it). The mod's share
    of that window is the init work C25 describes plus the C44 dump, and none of it has a duration recorded.
32. [ ] Decide what happens to the three options A18 shows never being called on this client: gate them,
    relabel them, or find the method this client actually uses. They are configured at 1000, 1000 and true
    and changed nothing across a whole career.
33. [ ] `SingleModeConfirmComplete` and `SingleModeResult` together are 26 s of a 362 s career. The auto skip
    path covers the result tween chains and printed nothing for the confirm screen. Read what the confirm
    screen spends its 13.4 s on in `hachimi.log` before writing a hook there.

34. [~] Measure the training cut-in before scaling anything in it. Run 8 recorded 104.7 s on
    `SingleModeMainView` and 39.7 s on the paddock, but it proved nothing about the cut-in: none of the
    installed training hooks printed a call line, and the log cannot say whether a friendship training even
    happened. A probe in the shape of `StoryFrameProbe` is what closes this: first-hit and totals for
    `IsTagTraining`, `PlayTrainingCutt`, `OnStartTrainingCutt`, `OnStopTrainingCutt`, `SkipRuntime`,
    `GetTargetSpeed`, `SingleModeTrainingCutInHelper.IsHighSpeedMode`, and the three `SingleModeUtils`
    getters in A20, plus the wall clock between the start and stop lines. Run 5 is the warning this item
    exists for.
35. [x] The classes that matter are not in the dump yet. `CLASS_FILTERS` in
    [src/il2cpp/introspect.rs](src/il2cpp/introspect.rs) has no match for `trainingcutt`, `tagtraining`,
    `trainingcutin` or `cuttcontroller`, so `TrainingCuttController`, `SingleModeTrainingCutInHelper` and
    `SingleModeMainViewTagTrainingCutInPlayer` appear only as name fragments in the metadata token index,
    with no signature. Filters alone are not enough (A26): the 500 full class cap is already spent, so the
     dump needs an exact name allowlist and a higher `MAX_FULL_CLASSES`, and the dump file it writes is
     already 1.94 MB per launch (C44). Built in `253458f` and not yet run: `FULL_DUMP_NAMES` now names 21
     classes for a full dump out of their own `MAX_ALLOWLIST_CLASSES = 40` budget, and `cutt` and `cutin`
     were added to `METHOD_FILTERS`. Run 9 ran it: the dump ends `527 classes (27 from the allowlist), 15157 methods,
     11238 fields written`, and `SingleModeMainTrainingCuttController` (dump L22929),
     `SingleModeMainViewTagTrainingCutInPlayer` (L23709), `SingleModeMainViewTrainingCutStatus` (L23742),
     `TrainingParamChangeUI` (L24571) and `Gallop.CutIn.Cutt.CutInTimelineController` (L26098) are there with full
     signatures, which is what A28, A29 and A30 are written from.
36. [ ] Pick the door with the measurement in hand. The game offers two shapes: its own skip
    (`SingleModeTrainingCutInHelper.SkipRuntime/0`, `ContextExtension.SkipRuntimeAll/1`,
    `ContextExtension.SkipPause/1`) which removes the animation, and its own rate
    (`GetTargetSpeed/0`, `SingleModeUtils.GetTrainingCutTimeScale/1`, `CutInBgModel.set_PlaySpeed/1`,
    `ContextExtension.SetTimeAll/2`) which shortens it. The AGENTS section 5 getter rule applies to
    `GetTargetSpeed`: check for a matching setter or a backing field before scaling a getter. Skipping
    outright also has to answer what happens to the flash labels that report the training result
    (`FLASH_LABEL_SPEED_UP_SUCCESS_IN`, `FLASH_LABEL_SPEED_UP_FAILURE_IN`) and to `WaitTapAsync`.

37. [~] Give the dump an exact class allowlist. The names worth a full signature are
    `SingleModeMainTrainingCuttController`, `SingleModeTrainingCutInHelper`, `SingleModeTrainingCutSettings`,
    `TagTrainingCutInPlayer`, `SingleModeMainViewTagTrainingCutInPlayer`,
    `SingleModeMainViewTrainingCutStatus`, `SingleModeMainViewTrainingCutStatusFrame`, `SingleModeUtils`,
    `SingleModeDefine`, `TrainingParamChangeUI`, `CutInTimelineController` and `TrainingCuttController`.
    Reaching them means an allowlist plus a higher `MAX_FULL_CLASSES`
    ([src/il2cpp/introspect.rs](src/il2cpp/introspect.rs)), because the cap is spent at
    `introspect.log:23504` while the image walk continues to L27524 (A26), and it has to be measured
    against C44: the dump already costs 1.94 MB of log inside the loader lock window. `253458f` implements the allowlist and the
    reserved budget, and the dump's last line now reads `N classes (M from the allowlist), ...` so a run
    can say which budget was spent. It is `[~]` until a launch shows the classes.
38. [~] Count calls, not changed values. The counting landed in `95bf6d1` and the probe report prints the
    five training scaling counters, so what is left here is the run. Before any training cut number is
    scaled, put first hit and
    periodic totals on `SingleModeUtils.GetTrainingCutTimeScale/1`,
    `SingleModeTrainingCutInHelper.GetTargetSpeed/0`, `SingleModeTrainingCutInHelper.IsHighSpeedMode/0`,
    `CutInTimelineController.SetSpeed/1` and `CutInTimelineController.UpdateSpeed/0`, and log a wall clock
    pair at the start and the end of the cut. `hit()` is silent when scaling changes nothing
    (`raw == scaled`, which includes a `0.0` duration), so the existing call lines cannot answer "was it
    called" (A21). This is also the run that settles whether a friendship training cut-in happened at all
    in run 8. Run 9 printed no call line for those five either, and their counters ride on the probe totals line from
    `v0.32.0-974da9c`.

39. [~] Rebuild the cut run boundary on the game's own state instead of a guessed method. `set_IsPlayingCutt/1` and
    `get_IsPlayingCutt/0` on `SingleModeMainTrainingCuttController` (A28) open and close a cut, and
    `GetTotalTime/0`, `GetTotalFrameCeil/0`, `get_CurrentFrame/0` and `get_Speed/0` on the timeline (A29) say how long
    the game thinks the cut is and how far it has got. Run 9 is the cost of not doing this first (C47). Attribute each
    run with `IsValidTag/2` (A28) so a friendship cut and a regular one stop sharing one number. All of it is installed
    in `19e741c` and run 10 read it: `PlayTrainingCut` 19, `CleanUpCutt` 35, `PlayIn` duration 5.640, while
    `set_IsPlayingCutt`, `get_IsPlayingCutt` and `IsValidTag` on both classes stayed silent (C49), so the boundary and
    the friendship split are still open.
40. [x] Re install the probe the matcher threw away. Accept `IL2CPP_TYPE_GENERICINST` in a reference parameter slot for
    the observe only probes so `TrainingParamChangeUI::InitializePlateList/2` resolves (C48). Landed in `0e98898`: the
    widening lives in `symbols::param_type_accepts`, runs only as a second pass behind the exact walk, and only the two
    new `resolve_generic_ref_method` / `resolve_static_generic_ref_method` profiles ask for it. Run 10 closed it: the
    install line printed, and the door reached 102 calls.
41. [~] Use the game's own skip and autoplay doors on the training cut before adding any multiplier. Run 9 counted 31
    `WaitTapAsync` and 31 `FadeOutResultFlash`, `Skip` asked 16 times with false, and the cut already runs at up to
    8.334. `IsAutoPlay/0` on the cutt controller, `get_IsAutoPlay/0` with `set_IsAutoPlay/1` on the timeline,
    `set_SkipFrame/1 -> void(int)`, and `field PLAY_SPEED_FOR_SKIP` beside `_forceTapWait` on `TrainingParamChangeUI`
    (A28, A29) are the paths the game itself uses. This is the same shape as the result screen auto skip in section 8
    item 4: go through the game's own skip, never past it. The doors are counted now, and nothing is written through
    them: the option was deliberately left unimplemented until a run says which door the game uses.
42. [~] Probe the story event the same way. `StoryEventProbe.rs` (`1840ef2`) measures the cut-in a story or story event
    plays through `Gallop.CutInHelper` (`Play`, `OnPlayCutIn`, `IsPlaying`, `GetTotalTime`, `GetTargetSpeed`,
    `FixedUpdateForHighSpeed(rate)`, `SetCurrentFrame`, `CleanupPlaying`) and the static extension doors in
    `Gallop.SingleModeTrainingCutHelperExtension.ContextExtension` (`SkipRuntimeAll`, `SkipPause`, `SetTimeAll`,
    `GetCurrentTime`, both `FixedUpdateForHighSpeed` overloads), attributed by view with a `story event screen` bucket
    added for `ViewId.StoryEventMission` 8102. It does not touch `Gallop.StoryViewController`, `StorySceneController`,
    `StoryEventMissionViewController` or `StoryCharacterFade`, because no dump has ever printed their signatures and a
    hook written from a name is what C47 is. Those four names are in the `debug_mode` allowlist (`eb92734`) so the next
    run delivers them. Run 9 spent only 7.8 s on Story out of 617.5 s, so the run that answers this has to be a session
    that actually plays a story event. Run 10 visited Story 3000 for 18.6 s over 13 visits and never opened
    `ViewId.StoryEventMission`, so the story event half is still unmeasured, and the half that did measure found the cut-in
    engine busy with its own entry door missing (C50).
43. [~] Find the door that opens a cut-in, on both sides, from a dump line rather than a name. The training pairing the
    run 10 data supports is `PlayTrainingCut(info)` open to `CleanUpCutt()` close (19 and 35 calls), and the story side has
    `CutInHelper::CleanupPlaying` closing 36 times with no opener at all (C49, C50). Nothing scales through a run, so this
    is measurement work only. Landed in `8a253e4` and `5754448`: the training pairing is in place, and the story probe now
    asks for the eight derived helper doors and the eight base lifecycle, pause, stop and status doors the dump names.
    Run 11 is the run that says whether any of them is the real one.
44. [ ] Measure the training screen's animations that are not the cut-in. `Gallop.SingleModeMainViewHpGauge` is dumped with
    `SetProgressbarBlendTime/1 -> void(float)`, `PlayIn/0`, `PlayValue/1`, `PlayValue/5`, `PlayMaxValue/2` and
    `field GAUGE_FADE_ANIMATION_DURATION [static const float]`; `Gallop.SingleModeLogItem` is dumped with
    `PlayInAnimation/0`, `PlayInsertAnimation/2 -> void(int, class<Action>)`, `PlayBalloonAnimation/0` and two
    `DG.Tweening.Sequence` fields; `Gallop.SingleModeMainViewTrainingFooter` has `GetCloseAnimWaitTime/0 -> static float()`
    and `PlayOutOnClick/0`. None of them is hooked or counted, and the training screen is 317.1 s of the 472 s career.
45. [ ] Put the training scaling on the doors the client actually calls. Run 10 printed all five `TRAINING_HIT_SLOTS` at
    zero for the third run running while `GetTargetSpeed` 598 calls and `GetTrainingCutTimeScale` 76 calls peaked at 11.280
    and `CutInTimelineController::UpdateSpeed` ran 2274 times (C51). Before anything is written through them: `GetTargetSpeed`
    and `UpdateSpeed` are getters on a timeline that has `SetSpeed/1 -> void(float)` and `set_SkipFrame/1 -> void(int)`, so
    the rule against scaling only a getter (AGENTS section 5) applies to whatever is built here.
46. [ ] Measure the game's own speed settings instead of inferring them. `StoryDefine::NORMAL_TIME_SCALE`,
    `HIGH_SPEED_TIME_SCALE_X4`, `HIGH_SPEED_TIME_SCALE_X16`, `HIGH_SPEED_TIME_SCALE_GRAND_LIVE`, `EVENT_WIPE_TIME_SCALE`,
    `INSPIRATION_WIPE_TIME_SCALE` and `STORY_END_TIME_SCALE` are `public static const float` in the dump, and
    `StoryManager::GetMaxHighSpeedType/0 -> static struct<HighSpeedType:4B>` is there too. Reading those fields once at
    init, with no hook and no write, would say what speed the story and event wipes already run at. `get_TargetFps` came
    back 20 for the training cut timeline in run 10, which is the first number of this kind the probe produced.
47. [~] Measure frames, not doors. Every probe in this fork counts calls, and a call count is not a cost: run 10 says
    317.1 s on the training screen without saying how much of it the game drew and how much a person read.
    `GameFrameProbe.rs` (`75d3782`) sits on the `GameSystem_Update` detour that already exists, measures the gap between
    two of the game's own ticks, and reports frames, total milliseconds, the worst frame and frames over 50 ms as stalls,
    per screen bucket. No new hook and no new game call, inert when `debug_mode` is off. It has never printed a line yet:
    the run that reads it is run 11.

### Probe build `253458f`, deployed and waiting for a career run (2026-10-08)

Deployed as `cri_mana_vpx.dll` at the game root, 29,352,448 bytes, SHA256
`4E4720D9140C8913A20B97AA4A4D1E3E87154E219CFC7FB94D80F6A1DC96B328`, built from the tree at `baa5570` so it
carries the C45 slider bound as well as the probe (the same tree before that commit hashed
`8C115D7C207A11F05D9321F600B48CAE13BD4C54E03AC665F8789F6221860038`), replacing the run 8 build (29,284,864
bytes, `2E36C270CCAE9B34B783D7DA5A1DE36F8987CD7CB107CD6BE3B0007F2BD8C88A`). Nothing in it changes
behaviour: every new hook hands its arguments to the original untouched, and they arm only under
`debug_mode`, which this client already has on.

What it adds:

- [src/il2cpp/hook/umamusume/TrainingCuttProbe.rs](src/il2cpp/hook/umamusume/TrainingCuttProbe.rs), 19
  observe only hooks over the training screen: the game's own rate query
  `SingleModeUtils.GetTrainingCutTimeScale/1` (static, so an argument only wrapper),
  `SingleModeTrainingCutInHelper.GetTargetSpeed/0`, `SkipRuntime/0`, `IsHighSpeedMode/0`, the cut-in
  engine's `ResetCurrentTime/0`, `get_CurrentTime/0`, `get_CurrentTimeScale/0`, `get_WaitingTime/0`,
  `SetSpeed/1`, `UpdateSpeed/0`, both `SkipRuntime` overloads and `SkipTimeDirect/1`, the training
  status `Skip/1`, `WaitTapAsync/0`, `FadeOutResultFlash/0`, `TrainingParamChangeUI.InitializePlateList/2`
  and the controller's `CoroutineDoTweenTimeScale/0` and `WaitTap/0`. Counts, not scaled values, which
  closes the `hit()` blind spot in A21. The frame hot getters do one increment and one atomic max and
  print nothing until a 4096 call chunk.
- Cut run measurement from `ResetCurrentTime`, the only proven start marker: a line per closed run with
  the peak of `get_CurrentTime` in the cut's own seconds and the wall clock between the two markers, plus
  running totals. `PlayTrainingCutt`, `OnStartTrainingCutt` and `OnStopTrainingCutt` are still
  signature-less, so no assumed name is hooked.
- Namespace aware class lookup: `class_for_label` splits a dump label at its last dot, which is how
  `Gallop.CutIn.Cutt.CutInTimelineController` resolves at all. A `Gallop.` lookup alone could not find
  it, which is A27.
- The dump allowlist described in item 37.
- Six unit tests over the peak merge, the millisecond conversion, the report gate, the totals line names
  and the label split. `cargo test --lib` is 65 passed, `cargo check --all-targets` clean, clippy clean on
  both legs.

### Two upgrades landed after the release tag (`95bf6d1` and `e372502`)

- `hit()` in `AnimationSpeed.rs` now counts every call to a scaling point and prints the first four even
  when the value did not move, then one line every 1024 calls. That removes the measurement half of A21:
  the five training hooks that printed install lines and no call line can now be read as reached or not
  reached, instead of inferred. `hit_calls(slot)` exposes the counter and `TRAINING_HIT_SLOTS` names the
  five training slots, which are the regular training path rather than the friendship one.
- The probe attributes each cut run to the view it started on, from `SceneManager::GetCurrentViewId` plus
  the scene id `AlterUpdate` already caches, so a training cut-in is separated from a race, gacha or story
  one. Buckets: training screen (1100, 1101, 1200, 1300, 1301), story (3000), gacha (300), the race scene
  family, and other. A run that opened in training stays attributed to training even if the cut-in that
  closed it happened on another screen.
- The probe report is three lines now: the per probe totals, `Cutt probe cut runs by screen:`, and
  `Cutt probe training scaling points reached:` carrying the five training scaling counters.
- Friendship training still cannot be told apart from regular training. `TagTraining`,
  `TagTrainingCutInPlayer` and `SingleModeMainViewTagTrainingCutInPlayer` have no dumped signatures, so
  nothing hooks them (A19), and both cases run through the same training cut-in classes, so they land in the
  same bucket. Separating them is the next dump's job, not a guessed signature.
- `cargo test --lib` is 68 passed, three new tests for the hit counter, the bucket attribution and the
  bucket names, and both clippy legs are clean with `-D warnings`.
- Deployed for the next run. The staged build `v0.32.0-e372502-dirty` was built while the README change was
  still uncommitted, so it was rebuilt from the clean tree and reports `v0.32.0-974da9c`, 29,357,056 bytes,
  SHA256 `FCA6E48B0C8800EA6A1FF396FDC8CB9F340AD2952BFFAF3A86C42993CD169463`, replacing the release build at
  the game root. The game was not running when the copy happened and `hachimi\config.json` was left alone.
  `debug_mode` is already true, so the probe arms with no config change.

What the next career run has to show before any training number is scaled, item 34:

1. `Cutt probe: X of 19 observe only probes installed` and the names in the "no class or no matching
   overload" line.
2. `Cutt probe: cut run N closed at T ms, timeline peak S s, wall W ms`, which is the first number this
   fork has ever had for the length of a friendship training cut-in.
3. `Cutt probe totals at N s:` with counts and peaks per probe, in particular
   `SingleModeUtils::GetTrainingCutTimeScale` and `SingleModeTrainingCutInHelper::GetTargetSpeed`, which
   say what rate the game already asks for.
4. `introspect.log` full dumps for the allowlisted classes, where `PlayTrainingCutt`,
   `OnStartTrainingCutt`, `_trainingCuttStartFrame`, `_trainingCuttEndFrame`, `_isTrainingCuttSkip` and
   the `TagTraining` holders should finally get signatures. The run also has to show the log did not blow
   up past a readable size (C44).
5. Whether a friendship training happened at all, which run 8 could not answer.

### Run 10, career on `v0.32.0-eb92734`, the probe's first real measurement (2026-10-08)

Build `v0.32.0-eb92734`, 29,502,464 bytes, SHA256
`BAE4343DCD07215F6F3DEDF24F82ED2BBBA45F14BB98282F7D84F6041F4D6C87`. Process started 20:05:38, log 233,592
bytes, first log line 00:05:44Z, last 00:20:01Z. `253 hooks armed in one pass, 0.075 s`, 10 `_addr is null`
lines in the known C30/C34/C39/C40 set, no panic, no new install failure beyond none: `Cutt probe: 50 of 47
observe only probes installed` (50 doors installed, 47 of them carry a counted probe, three feed the timeline
self report) and `Story event probe: 14 of 14 observe only hooks installed`. Config snapshot unchanged.

The career itself ran 00:05:46 to 00:13:38, 472 s. The rest of the log is the profile screen afterwards.

| view | id | wall | visits |
|---|---|---|---|
| single mode training screen | 1101 | 317.1 s | 16 |
| paddock | 1200 | 55.8 s | 9 |
| race program detail | 1301 | 24.1 s | 1 |
| story | 3000 | 18.6 s | 13 |
| home hub | 101 | 16.3 s | 2 |
| race | 400 | 14.7 s | 1 |
| race program | 1300 | 10.2 s | 1 |
| month start | 1100 | 7.5 s | 6 |
| succession event | 1501 | 5.3 s | 1 |
| loading | 2 | 3.7 s | 1 |
| title | 1 | 2.1 s | 1 |

36 `NowLoading::PlayFadeNowLoading` pairs, every duration 0.3 s, the clamped floor, nothing above it.

**The game's own training cut numbers, first time in this fork's history:**

```
SingleModeMainTrainingCuttController::PlayTrainingCut(info)=19
SingleModeMainTrainingCuttController::PlayTrainingCutEndAsync(id, flag, flag)=19
SingleModeMainTrainingCuttController::CleanUpCutt()=35
SingleModeMainTrainingCuttController::WaitTapAsync()=19
SingleModeMainTrainingCuttController::FadeOutResultFlash()=19
CutInTimelineController::GetTotalTime()=19 peak 5.640
SingleModeMainViewTrainingCutStatus::PlayIn(duration, count, flag, action)=38 peak 5.640
SingleModeMainViewTrainingCutStatus::PlayOut(flag, action)=22
SingleModeMainViewTagTrainingCutInPlayer::PlayCutIn(cards, done)=18
SingleModeMainViewTagTrainingCutInPlayer::PlayCutInOut(done)=18
TrainingParamChangeUI::InitializePlateList(list, interval)=102 peak 1.000
CutInTimelineController::UpdateSpeed()=2274
CutInTimelineController::SkipRuntime(time)=50
SingleModeTrainingCutInHelper::GetTargetSpeed()=598 peak 11.280
SingleModeUtils::GetTrainingCutTimeScale(scale)=76 peak 11.280
SingleModeTrainingCutInHelper::IsHighSpeedMode()=39
CutInTimelineController::get_TargetFps() read 20
```

A training cut is 5.640 s of timeline authored at 20 fps, 19 of them in this career, each ended by
`CleanUpCutt`, each with 18 tag cut-ins alongside. `GetTrainingCutTimeScale` again returned double its
argument (5.640 in, 11.280 out), matching run 9's `[3.04, 6.08]`.

**C49. The run boundary chosen for C47 is also not a door this client uses.** `cut runs 0` for the whole
career while the 19 cuts above ran, because `set_IsPlayingCutt(playing)` and `get_IsPlayingCutt()` are both
silent, as are `UpdateTrainingCutIn`, `FixedUpdateTrainingCutIn`, `LateUpdateTrainingCutIn`,
`TrainingAsync`, `PlayScenarioTrainingCut`, `PlayTrainingSaboriAsync`, `IsValidTag` on both classes,
`set_SkipFrame`, `set_IsAutoPlay`, `get_IsAutoPlay`, `IsAutoPlay`, `SetSpeed`, `SkipTimeDirect`,
`SkipRuntime(frames, keep)`, `get_CurrentTime`, `get_CurrentTimeScale`, `get_WaitingTime`,
`ResetCurrentTime`, `GetTotalFrameCeil`, `get_CurrentFrame`, `WaitTap`, `CoroutineDoTweenTimeScale`,
`ExistPlayingFrame`, `GetIntervalOutBegine`, `WillRankUpInHighSpeedMode`. 29 of 47 counted doors printed no
call line at all. The pairing that the data supports is `PlayTrainingCut` open to `CleanUpCutt` close.

**C50. The story event probe's open door is not `CutInHelper::Play` either.** `Play(timeline, parent)` and
`OnPlayCutIn(timeline, parent)` are silent, `cut-ins 0`, while the cut-in engine was busy all career:

```
CutInHelper::IsPlaying()=54 peak 1.000
CutInHelper::GetTotalTime()=757 peak 5.667
CutInHelper::CleanupPlaying()=36
ContextExtension::SetTimeAll(helpers, time)=1512 peak 5.667
ContextExtension::SkipPause(helpers)=756
ContextExtension::FixedUpdateForHighSpeed(helpers)=4769
ContextExtension::FixedUpdateForHighSpeed(helpers, rate)=598 peak 11.280
ContextExtension::SkipRuntimeAll(helpers)=4
```

`ContextExtension::GetCurrentTime`, `CutInHelper::GetTargetSpeed`, `CutInHelper::FixedUpdateForHighSpeed(rate)`
and `CutInHelper::SetCurrentFrame` are silent.

**C51. The training scaling doors the mod already installs were never called.**
`TrainingFooter.GetItemAnimDuration=0 TrainingFooter.GetCloseAnimWaitTime=0 TrainingCuttClip.get_DelayTime=0
SingleModeUtils.GetHighSpeedPlayDuration=0 SingleModeUtils.GetCutTimeScale=0`. Whatever this fork has been
scaling on the training screen is not the path the client takes; the path it takes runs through
`GetTargetSpeed`, `GetTrainingCutTimeScale` and `CutInTimelineController::UpdateSpeed`, which are measured and
not scaled.

Story stayed quiet in this career, as its dwell shows: `IsStoryEndFrameOrGrandLiveWaitFrameSkipped=10284`,
`get_WaitFrameCountUntilNextBlock=556`, `UpdateTimeScaleByHispeedType=229`, all the wait frame setters 0,
story high speed mode engaged 0, 7 writes. No story event screen was visited, so the story event doors still
have no data.

The allowlist delivered what it was for. `introspect.log` for this run is 1,966,611 bytes for
534 classes (34 from the allowlist), 15,772 methods, 11,455 fields, and it now contains full blocks for
`Gallop.StoryViewController` (418 lines), `Gallop.StorySceneController` (133),
`Gallop.StoryEventMissionViewController` (640), `Gallop.StoryCharacterFade` (61),
`Gallop.SingleModeMainViewController`, `Gallop.SingleModeMainViewHpGauge` (74) and
`Gallop.SingleModeLogItem` (109). C44 is still open: this is 1.97 MB at launch, and the next allowlist
increase has to be watched against it.



### Probe v3 build `75d3782`, deployed for run 11 (2026-10-08 20:41)

Three code commits, one per concern, all built from the tree at `75d3782`:

- `8a253e4` pairs a training cut run on the doors run 10 proved the client calls. `PlayTrainingCut`,
  `PlayScenarioTrainingCut` and `PlayTrainingSaboriAsync` open a run and `CleanUpCutt` closes it.
  `set_IsPlayingCutt` and `get_IsPlayingCutt` are still counted and no longer drive anything, and the install line
  now reads `{installed} doors installed, 47 of them carry a counted kind` instead of the confusing `50 of 47`.
- `5754448` puts the story probe on the doors the C50 dump named. On `Gallop.CutInHelper`: `OnStart`, `OnEnd`,
  `OnEndCutIn`, `OnRestart`, `StopRequest`, `Pause(play, pause)`, `IsPause`, `set_Status(status)`. On
  `Gallop.SingleModeTrainingCutInHelper`: `Play`, `OnPlayCutIn`, `OnStartCutIn`, `OnPlayMainCutIn`, `OnEndCutIn`,
  `CleanupPlaying`, `OnTerminateRuntime(controller)`, `OnTimelineUpdatePost(time)`. 14 doors became 30. Each run
  now prints which door opened it and which closed it, and a run keeps the first open it gets so two openers
  cannot move the start clock to the later one. `set_Status` takes
  `struct<Gallop.CutInHelper.CutInStatus:4B>`, declared as one word and handed back untouched, on the strength of
  A5, and its values are sampled, which is how the cut-in's own state machine stops being a guess.
- `75d3782` adds `GameFrameProbe.rs`, a frame clock on the `GameSystem_Update` detour the fork already owns. No
  new hook and no new game call: the gap between two of the game's own ticks is one rendered frame. It counts
  frames, total milliseconds, the worst frame, and the frames over 50 ms as stalls, per screen bucket, and it
  does nothing at all when `debug_mode` is off. The 50 ms line is the 20 fps the run 10 log read out of
  `get_TargetFps`: a frame past that is a frame the cut timeline already lost. This is what separates the time a
  screen spends drawing from the time a person spends reading.

`cargo test --lib` is 79 passed. `cargo check --all-targets`, both clippy legs with `-D warnings` and
`cargo build --release` exit 0. No review pass was run, per the agreed order: smoke test now, thorough review
after the features.

Deployed as `cri_mana_vpx.dll` at the game root: 29,554,688 bytes, SHA256
`C30F3E692E4024D4C341D8044AF2B3708079EE010EBF4BD44670C60C3C85AAAD`, version string `0.32.0-75d3782`. It replaces
`v0.32.0-eb92734` (29,502,464 bytes, `BAE4343DCD07215F6F3DEDF24F82ED2BBBA45F14BB98282F7D84F6041F4D6C87`), kept at
`target\deploy-backup\cri_mana_vpx-eb92734.dll`. The game was not running when the copy happened and
`hachimi\config.json` is untouched at 6,740 bytes from 08:33.

What run 11 has to show:

1. `Cutt probe: cut run N closed at ... as K` lines. If they are still absent while `PlayTrainingCut` shows
   calls, the cut does not end at `CleanUpCutt` either, and that is a third finding rather than a fast game.
2. `Story event probe: cut-in N closed after T ms from DOOR to DOOR` lines, which is the only way C50 closes.
   `SingleModeTrainingCutInHelper::Play` and `OnPlayCutIn` are the doors the dump names; if they are silent too,
   the cut-in is opened from somewhere the dump has not printed yet.
3. Whether `CutInHelper::Pause`, `IsPause` or `StopRequest` is ever reached. They are the game's own pause and
   stop doors on this screen, and item 41 has been looking for exactly that.
4. `set_Status` values, which is the cut-in state machine read as numbers instead of inferred from method names.
5. `Frame clock totals` and `Frame clock by screen`, which is the first measurement in this fork that is not a
   door count: frames, worst frame and stalls per screen. Compare the training screen's frame total against its
   317.1 s of dwell from run 10.
6. `training scaling points reached` for the five `TRAINING_HIT_SLOTS` doors. If they stay at zero (C51) then any
   training speed option this fork writes has to sit on `GetTargetSpeed`, `GetTrainingCutTimeScale` or
   `UpdateSpeed`, and `GetTargetSpeed` has a setter beside it, so the rule against scaling a getter alone applies.

In the fix order list, item 43 moves from `[ ]` to `[~]` (the doors are found and installed; the run that reads
them has not happened) and item 47 is new at `[~]` (the frame clock is written and deployed, and has never printed a
line). Items 39, 41 and 42 stay `[~]`, items 44, 45 and 46 stay `[ ]`. The list now reads 12 `[x]`, 17 `[~]`,
8 `[ ]`, and the ledger bullet counts are 12 `[x]`, 21 `[~]`, 54 `[ ]`, 3 `[latent]`.


### The build run 10 carried (deployed 2026-10-08 19:56)

Four commits, one per concern: `0e98898` (the generic parameter matcher), `19e741c` (the training cut probe
rebuilt on the doors the run 9 dump named), `1840ef2` (`StoryEventProbe.rs` and its registration), `eb92734`
(the story classes added to the dump allowlist).

Deployed as `cri_mana_vpx.dll` at the game root: `v0.32.0-eb92734`, 29,502,464 bytes, SHA256
`BAE4343DCD07215F6F3DEDF24F82ED2BBBA45F14BB98282F7D84F6041F4D6C87`, replacing `v0.32.0-974da9c` (29,357,056
bytes, `FCA6E48B0C8800EA6A1FF396FDC8CB9F340AD2952BFFAF3A86C42993CD169463`). The game was not running when the
copy happened and `hachimi\config.json` was left alone. The release asset at the `v0.32.0-fork.1` tag stays the
`d42b6d9` build; a fork.2 release is still deferred until a run has read this one.

Nothing in any of it changes behaviour. Every hook hands its arguments to the original untouched, all of them
arm only under `debug_mode`, and no new option was added: the scope for this step was measurement only, so the
game's own skip and autoplay doors are counted rather than driven (item 41).

What the training probe became, from 19 probes to 47:

- The run boundary is `set_IsPlayingCutt/1 -> void(bool)`, the game's own statement that a cut is playing. An
  open while one is already open closes the running one first, and a close with nothing open is dropped instead
  of inventing a run. `ResetCurrentTime` stays counted, because run 9 proved a training cut never reaches it
  (C47).
- The friendship split is measured instead of guessed: `IsValidTag/2` on the cutt controller and the static
  `IsValidTag/1` on `SingleModeMainViewTagTrainingCutInPlayer` record what the game answered, and
  `PlayCutIn/2` on that player decides the kind of the cut that is open, because that is the door a friendship
  cut is played through. A cut whose answer the game never gave is reported as unknown rather than called
  regular. A new totals line prints `cut runs by kind` with the wall time of each.
- The timeline is read for its own numbers: `GetTotalTime`, `GetSpeed`, `GetTotalFrameCeil`, `get_CurrentFrame`
  and `get_TargetFps` (A29), so a length can come from the game instead of being inferred from wall clock. The
  skip doors `set_SkipFrame/1` and `set_IsAutoPlay/1` are counted to answer whether the game uses them at all
  (A30).
- The doors the mod already scales on the training screen still ride on the totals line, and the new status
  panel doors (`PlayIn/4` with its duration argument, `PlayOut/2`, `GetIntervalOutBegine/1`,
  `WillRankUpInHighSpeedMode/0`, `ExistPlayingFrame/0`) are measured for the first time.
- Labels in the totals line keep their argument list. `SkipRuntime(time)` and `SkipRuntime(frames, keep)` are two
  different doors and the earlier line printed both under one name; the story probe's test caught the same
  collision for its two `FixedUpdateForHighSpeed` overloads before it ever reached a log.
- Buckets gained `story event screen` for `ViewId.StoryEventMission` 8102, so a story event cut-in is no longer
  reported as "other".

What the story event probe measures, 14 hooks, all of them signatures read out of the run 9 dump:
`Gallop.CutInHelper` `Play`, `OnPlayCutIn`, `IsPlaying`, `GetTotalTime`, `GetTargetSpeed`,
`FixedUpdateForHighSpeed(rate)`, `SetCurrentFrame`, `CleanupPlaying`, and the static doors in
`Gallop.SingleModeTrainingCutHelperExtension.ContextExtension` `SkipRuntimeAll`, `SkipPause`, `SetTimeAll`,
`GetCurrentTime` and both `FixedUpdateForHighSpeed` overloads. Its wall clock run is explicitly labelled as
measured from `Play` to `CleanupPlaying`, because that is the pairing it is, and a close that lands before its
open is dropped. The generic list parameters are exactly what item 40 unlocked: they report `GENERICINST`, and
the resolver that answers them is the one whose wrapper declares a pointer.

`cargo test --lib` is 76 passed. Both clippy legs are clean with `-D warnings`. No full review pass was run on
this step; it is scheduled for after the feature work, so the ABI and pairing claims above are backed by the
dump and the tests, not yet by a reviewer.

What run 10 answered from that list:

1. Answered. `Cutt probe: 50 of 47 observe only probes installed` (50 doors, 47 counted, three feeding the self
   report), no "no class or no matching overload" line at all, and `InitializePlateList` installed and reached.
2. Answered, and negatively: `cut runs 0` while the cuts ran, and `set_IsPlayingCutt` never showed a call (C49).
3. Not answered. `Cutt probe cut kinds:` printed no kind, because `IsValidTag` on both classes was silent and no
   run closed to attribute.
4. Answered. `GetTotalTime()=19 peak 5.640` and `TrainingCutStatus::PlayIn` duration peak 5.640, with
   `get_TargetFps` read as 20. `get_Speed` was silent.
5. Partly answered. `Story event probe: 14 of 14 observe only hooks installed` and its totals lines printed, but no
   story event screen was opened, and the cut-in entry door is missing (C50).
6. Answered. 1,966,611 bytes for 534 classes with 34 allowlisted, and the four story classes present.

Bracketed status after this edit: 12 `[x]`, 21 `[~]`, 54 `[ ]`, 3 `[latent]` as bullet items. In the fix order list
item 40 moved to `[x]` on the strength of the run, items 39, 41 and 42 stay `[~]`, and items 43 to 46 are new `[ ]`.
The fix order list now reads 12 `[x]`, 15 `[~]`, 9 `[ ]`.

## E. Merge with upstream v0.32.0 (`5f89a7e`)

Merge base `83dd99f` (upstream #231). 11 upstream commits, 38 files, 1845 insertions, 136 deletions.
One textual conflict, in `src/il2cpp/mod.rs`: upstream made `utils` public while we had added
`introspect` and kept `utils` private, resolved by taking both. Every other overlap (`gui.rs`,
`hachimi.rs`, seven locale files, `GameSystem.rs`, `SceneManager.rs`, `umamusume/mod.rs`,
`UnityEngine_CoreModule/mod.rs`) merged without intervention, and the merged tree builds with no
errors and no warnings.

What arrived:

- JP only shadow options `shadow_distance`, `soft_shadows`, `soft_shadow_quality`,
  `shadow_depth_bias`, `shadow_normal_bias`, `force_chara_shadows` and `story_shadow_type`, together
  with new modules `StoryTimelineBg3DClipData`, `CascadeShadow`, `CascadeShadowForRace`,
  `GallopRenderer`, `Shader` and `ScreenSpaceAmbientOcclusion`. They are region gated to Japan, so on
  this Global client they are inert.
- Standalone custom font support: `custom_font_file` replaces `custom_font_asset_bundle` and
  `custom_font_name`, `.hachifont` ZIP packs are scanned every 3 s while the Config Editor is open,
  bundles load through `AssetBundle.LoadFromMemoryAsync_Internal`, with a 2 GB bundle limit, a 4 KB
  `font_path.txt` limit and a one time `custom_font_file_warning`.
- `SceneManager` caches `GetCurrentSceneId` from an `AlterUpdate` detour and exposes
  `is_race_scene_family()`. Its new wrapper inherits the zero address guard our macro family already
  applies. Our speed groups stay installed last in `umamusume::init`.
- `GameSystem` writes `_isMSAA` when msaa is enabled, the race playback slider and button no longer
  persist as invisible unclickable areas after a race, and race finish detection for overlays is fixed.
- `tl_repo::download_incremental` writes to a `.part` file, localized data reloads on an update
  failure, corrupted texture diffs are deleted in favour of the original game texture, and
  `run_internal` returns `RuntimeError` instead of panicking on an unset TL repo directory or id.

- [ ] **E1 Upstream locale duplicates.** `ko.yml` defines `shadow_depth_bias` and `shadow_normal_bias`
  twice inside `config_editor`, `zh-cn.yml` defines `custom_font_file` and `custom_font_none_found`
  twice. The earlier definition is never read.
- [ ] **E2 The new option labels are untranslated in the locales this fork maintains.** The nine here
  are upstream's labels: `shadow_distance`, `soft_shadows`, `soft_shadow_quality`, `shadow_depth_bias`,
  `shadow_normal_bias`, `force_chara_shadows`, `story_shadow_type`, `custom_font_file` and
  `custom_font_none_found`, plus the `custom_font_file_warning` text block. They are still absent, and
  fall back to English, in `es`, `id` and `vi` (none of the nine), in `fil` (`soft_shadows`,
  `soft_shadow_quality`, `shadow_depth_bias`, `shadow_normal_bias`; what fil has instead is
  `soft_shadows_quality` at `fil.yml:248`, a key no code asks for) and in `zh-tw` (all seven shadow
  keys and `custom_font_file_warning`). `performance_tab` is present in all ten. `story_high_speed_mode`
  is present in all ten, in English for `en` and in new machine translations for the rest, which nobody
  has proofread.
  Corrected against the tree this change set produced. The earlier version of this entry said `es`, `id`
  and `vi` carry none of the new keys, and that no longer describes the locales: the six labels this fork
  added, `time_scale`, `transition_speed`, `result_screen_speed`, `story_speed`, `auto_skip_result_screens`
  and `high_speed_settings`, are now in all ten, one definition each under `config_editor`
  (`en.yml:218-223`, `es.yml:203-208`, `id.yml:177-182`, `vi.yml:139-144`), written by the E3 fix. Do not
  add those six again. A second definition is what E1 reports for `ko.yml`'s `shadow_depth_bias` and
  `shadow_normal_bias`, and the earlier one is never read. The stale half of this entry also named
  `story_skip_frame_scale` as present in all ten. It is in none of them: the option, its row and its label
  were dropped in `a2c5f67`, `30e1cb4` and `8505cfa`, and the only mention left in this ledger is the
  value a config snapshot quoted in the run 5 note in section A. Checked by counting every key at its own
  line in each of the ten files, not by a run. Still open here: the upstream list above, and the fork
  labels E3 records as not touched, `target_fps_unfocused` in `es`, `id`, `ko`, `vi` and `zh-tw`,
  `cyspring_mono_uncap_frame_scale` in `fil`, `vi` and `zh-tw`, and `ui_animation_scale` in `vi`. Closing
  it needs translations somebody has proofread and a run that shows the Config Editor drawing them in the
  language `locale` selects.
- [~] **E3 This fork's own Performance labels were English-only in nine locales.** `time_scale`,
  `transition_speed`, `result_screen_speed`, `story_speed`, `auto_skip_result_screens` and
  `high_speed_settings` were defined only in `en.yml:218-223`, while the Performance tab in
  `src/core/gui.rs` renders all six, so with `fallback = "en"` (`src/lib.rs:6`) every non-English
  client showed English for the options this fork added. E2 above is a separate gap: it covers
  upstream's shadow and font keys. Fix applied in source, not verified by a run: the six keys are now
  in `es`, `fil`, `id`, `ko`, `pt-br`, `ru`, `vi`, `zh-cn` and `zh-tw`, written between
  `ui_animation_scale` and `story_high_speed_mode` in the order the tab renders them (`vi.yml` has no
  `ui_animation_scale`, so it is anchored on `story_high_speed_mode`). Machine translations, nobody
  has proofread them. Verified so far: the ten locale files parse as YAML, each of the six keys sits
  once under `config_editor` and none of the nine is identical to its `en` text, and the six
  `t!("config_editor.<key>")` labels the tab draws are at `src/core/gui.rs:5895-5929`. A scratch
  `rust-i18n` probe crate resolved the keys and `cargo check` was clean; the probe is not in the tree,
  so that part is not reproducible here. No game run yet, so it stays `[~]`. What a run has to show is
  the Config Editor Performance tab drawing these six labels in the language set by `locale` rather
  than falling back to English. Closing the item also needs the `l10n` commit hash that carries this
  change, and that hash is `4e40dbf`: every one of the ten locale files now carries these six labels
  exactly once, each under `config_editor`, checked line by line in this tree. Not
  touched here: `target_fps_unfocused` is still missing from `es`, `id`, `ko`, `vi` and `zh-tw`,
  `cyspring_mono_uncap_frame_scale` from `fil`, `vi` and `zh-tw`, `ui_animation_scale` from `vi`, and
  `ko.yml` and `zh-cn.yml` still carry the E1 duplicate keys.
- A6, A13, C22, C23 and C24 are untouched by this merge. Upstream changed nothing in `NowLoading.rs`,
  and its `StoryTimelineData.rs` edit is the JP `rewrite_story_shadow_types` path.

## F. Release v0.32.0-fork.1 (2026-10-08)

First release on this fork's repository, `John-UNOwen/Hachimi-Edge`. The repo had no tags and no releases
before it. The 51 tags in the local clone, `v0.14.1` through `v0.32.0`, are upstream's, fetched from
`kairusds/Hachimi-Edge`.

- Tag `v0.32.0-fork.1`, annotated, on commit `d42b6d9`, covering 62 non-merge commits of fork work since
  the upstream v0.32.0 merge at `5f89a7e`. `main` and the tag were both pushed.
- Release page: https://github.com/John-UNOwen/Hachimi-Edge/releases/tag/v0.32.0-fork.1, marked
  pre-release because the training cut-in is still unmeasured (fix order item 34) and the Android legs C42
  and C43 have never been run on a device.
- Asset `hachimi-edge-0.32.0-fork.1-windows.zip`, 15,278,813 bytes, SHA256
  `24848DAB0F0478606F6FD2A5119190DAA8F5F496D92B31174AD4AB21532DBFD4`, holding `cri_mana_vpx.dll` at
  29,352,448 bytes SHA256 `1E689C12106242ACFA02E981284375D4B71313CDF93AC76029DD34F5226D673B` and an
  `INSTALL.txt`.
- The DLL reports `v0.32.0-d42b6d9`. That is `build.rs:28-62` writing `v{CARGO_PKG_VERSION}` plus
  `git rev-parse --short HEAD`, with no `-dirty` because the tree was clean at build time.
- The same binary was deployed at the game root, and is no longer: the tree moved on with the probe
  upgrades, so the game root now holds `v0.32.0-974da9c`. The release asset stays the build above, and a
  `v0.32.0-fork.2` tag waits until the training cut-in work closes.
- The English README now shows `assets/screenshot-performance.png`, committed in `d3f2c42`.
  `assets/screenshot-1.png` and `screenshot-2.png` are kept because `README-zh_cn.md` and `README-zh_tw.md`
  still reference them.
- Built on this machine after the checks the release text claims: `cargo test --lib` 65 passed,
  `cargo check --all-targets` clean, clippy clean on `x86_64-pc-windows-msvc --all-targets` and
  `aarch64-linux-android --all-targets` with `-D warnings`, `cargo build --release` 2 m 15 s.
- GitHub Actions has never run on this fork. `GET /repos/John-UNOwen/Hachimi-Edge/actions/runs` returns
  `total_count: 0` while all three workflows are listed `active` and the repo reports
  `allowed_actions: all`. Actions is off by default on a fork of a public repository, and switching it on
  is a settings action in the web UI. Until that happens, every CI claim about this fork is a local run
  like the one above and not a CI result.
- Not used, deliberately: `.github/workflows/create_release.yml`. It is `workflow_dispatch` only, it names
  the release from `Cargo.toml` which would be plain `v0.32.0`, the same name upstream already used, and its
  Windows job also builds Cellar, FunnyHoney and `hachimi_installer.exe` out of four pinned third party
  repositories along with an Android `.so` this fork has never run on a device. If a future release wants
  the installer path, that workflow is the place, and the version in `Cargo.toml` has to be bumped first.
