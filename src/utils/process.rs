use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use sysinfo::{Pid, Process, ProcessExt, SystemExt};

use lazy_static::lazy_static;

#[derive(Clone, Debug, PartialEq)]
pub enum CallingProcess {
    GitShow(String),                             // (extension)
    GitGrep((HashSet<String>, HashSet<String>)), // ((long_options, short_options))
    OtherGrep,                                   // rg, grep, ag, ack, etc
}

pub fn calling_process() -> Option<Cow<'static, CallingProcess>> {
    #[cfg(not(test))]
    {
        CACHED_CALLING_PROCESS
            .as_ref()
            .map(|proc| Cow::Borrowed(proc))
    }
    #[cfg(test)]
    {
        determine_calling_process().map(|proc| Cow::Owned(proc))
    }
}

lazy_static! {
    static ref CACHED_CALLING_PROCESS: Option<CallingProcess> = determine_calling_process();
}

fn determine_calling_process() -> Option<CallingProcess> {
    calling_process_cmdline(ProcInfo::new(), describe_calling_process)
}

// Return value of `extract_args(args: &[String]) -> ProcessArgs<T>` function which is
// passed to `calling_process_cmdline()`.
#[derive(Debug, PartialEq)]
pub enum ProcessArgs<T> {
    // A result has been successfully extracted from args.
    Args(T),
    // The extraction has failed.
    ArgError,
    // The process does not match, others may be inspected.
    OtherProcess,
}

pub fn git_blame_filename_extension() -> Option<String> {
    calling_process_cmdline(ProcInfo::new(), guess_git_blame_filename_extension)
}

pub fn guess_git_blame_filename_extension(args: &[String]) -> ProcessArgs<String> {
    let all_args = args.iter().map(|s| s.as_str());

    // See git(1) and git-blame(1). Some arguments separate their parameter with space or '=', e.g.
    // --date 2015 or --date=2015.
    let git_blame_options_with_parameter =
        "-C -c -L --since --ignore-rev --ignore-revs-file --contents --reverse --date";

    let selected_args =
        skip_uninteresting_args(all_args, git_blame_options_with_parameter.split(' '));

    match selected_args.as_slice() {
        [git, "blame", .., last_arg] if is_git_binary(git) => match last_arg.split('.').last() {
            Some(arg) => ProcessArgs::Args(arg.to_string()),
            None => ProcessArgs::ArgError,
        },
        [git, "blame"] if is_git_binary(git) => ProcessArgs::ArgError,
        _ => ProcessArgs::OtherProcess,
    }
}

pub fn describe_calling_process(args: &[String]) -> ProcessArgs<CallingProcess> {
    let mut args = args.iter().map(|s| s.as_str());

    fn is_any_of<'a, I>(cmd: Option<&str>, others: I) -> bool
    where
        I: IntoIterator<Item = &'a str>,
    {
        cmd.map(|cmd| others.into_iter().any(|o| o.eq_ignore_ascii_case(cmd)))
            .unwrap_or(false)
    }

    match args.next() {
        Some(command) => match Path::new(command).file_stem() {
            Some(s) if s.to_str().map(|s| is_git_binary(s)).unwrap_or(false) => {
                let mut args = args.skip_while(|s| *s != "grep" && *s != "show");
                match args.next() {
                    Some("grep") => {
                        ProcessArgs::Args(CallingProcess::GitGrep(parse_command_option_keys(args)))
                    }
                    Some("show") => {
                        if let Some(extension) = get_git_show_file_extension(args) {
                            ProcessArgs::Args(CallingProcess::GitShow(extension.to_string()))
                        } else {
                            // It's git show, but we failed to determine the
                            // file extension. Don't look at any more processes.
                            ProcessArgs::ArgError
                        }
                    }
                    _ => {
                        // It's git, but not a subcommand that we parse. Don't
                        // look at any more processes.
                        ProcessArgs::ArgError
                    }
                }
            }
            // TODO: parse_style_sections is failing to parse ANSI escape sequences emitted by
            // grep (BSD and GNU), ag, pt. See #794
            Some(s) if is_any_of(s.to_str(), ["rg", "ack", "sift"]) => {
                ProcessArgs::Args(CallingProcess::OtherGrep)
            }
            Some(_) => {
                // It's not git, and it's not another grep tool. Keep
                // looking at other processes.
                ProcessArgs::OtherProcess
            }
            _ => {
                // Could not parse file stem (not expected); keep looking at
                // other processes.
                ProcessArgs::OtherProcess
            }
        },
        _ => {
            // Empty arguments (not expected); keep looking.
            ProcessArgs::OtherProcess
        }
    }
}

fn get_git_show_file_extension<'a>(args: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    if let Some(last_arg) = skip_uninteresting_args(args, "".split(' ')).last() {
        // E.g. "HEAD~1:Makefile" or "775c3b8:./src/delta.rs"
        match last_arg.split_once(':') {
            Some((_, suffix)) => suffix.split('.').last(),
            None => None,
        }
    } else {
        None
    }
}

fn is_git_binary(git: &str) -> bool {
    // Ignore case, for e.g. NTFS or APFS file systems
    Path::new(git)
        .file_stem()
        .and_then(|os_str| os_str.to_str())
        .map(|s| s.eq_ignore_ascii_case("git"))
        .unwrap_or(false)
}

// Skip all arguments starting with '-' from `args_it`. Also skip all arguments listed in
// `skip_this_plus_parameter` plus their respective next argument.
// Keep all arguments once a '--' is encountered.
// (Note that some arguments work with and without '=': '--foo' 'bar' / '--foo=bar')
fn skip_uninteresting_args<'a, 'b, ArgsI, SkipI>(
    mut args_it: ArgsI,
    skip_this_plus_parameter: SkipI,
) -> Vec<&'a str>
where
    ArgsI: Iterator<Item = &'a str>,
    SkipI: Iterator<Item = &'b str>,
{
    let arg_follows_space: HashSet<&'b str> = skip_this_plus_parameter.into_iter().collect();

    let mut result = Vec::new();
    loop {
        match args_it.next() {
            None => break result,
            Some("--") => {
                result.extend(args_it);
                break result;
            }
            Some(arg) if arg_follows_space.contains(arg) => {
                let _skip_parameter = args_it.next();
            }
            Some(arg) if !arg.starts_with('-') => {
                result.push(arg);
            }
            Some(_) => { /* skip: --these -and --also=this */ }
        }
    }
}

// Given `--aa val -bc -d val e f -- ...` return
// ({"--aa"}, {"-b", "-c", "-d"})
fn parse_command_option_keys<'a>(
    args: impl Iterator<Item = &'a str>,
) -> (HashSet<String>, HashSet<String>) {
    let mut longs = HashSet::new();
    let mut shorts = HashSet::new();

    for s in args {
        if s == "--" {
            break;
        } else if s.starts_with("--") {
            longs.insert(s.split('=').next().unwrap().to_owned());
        } else if let Some(suffix) = s.strip_prefix('-') {
            shorts.extend(suffix.chars().map(|c| format!("-{}", c)));
        }
    }
    (longs, shorts)
}

struct ProcInfo {
    info: sysinfo::System,
}
impl ProcInfo {
    fn new() -> Self {
        ProcInfo {
            info: sysinfo::System::new(),
        }
    }
}

trait ProcActions {
    fn cmd(&self) -> &[String];
    fn parent(&self) -> Option<Pid>;
    fn start_time(&self) -> u64;
}

impl<T> ProcActions for T
where
    T: ProcessExt,
{
    fn cmd(&self) -> &[String] {
        ProcessExt::cmd(self)
    }
    fn parent(&self) -> Option<Pid> {
        ProcessExt::parent(self)
    }
    fn start_time(&self) -> u64 {
        ProcessExt::start_time(self)
    }
}

trait ProcessInterface {
    type Out: ProcActions;

    fn my_pid(&self) -> Pid;

    fn process(&self, pid: Pid) -> Option<&Self::Out>;
    fn processes(&self) -> &HashMap<Pid, Self::Out>;

    fn refresh_process(&mut self, pid: Pid) -> bool;
    fn refresh_processes(&mut self);

    fn parent_process(&mut self, pid: Pid) -> Option<&Self::Out> {
        self.refresh_process(pid).then(|| ())?;
        let parent_pid = self.process(pid)?.parent()?;
        self.refresh_process(parent_pid).then(|| ())?;
        self.process(parent_pid)
    }
    fn naive_sibling_process(&mut self, pid: Pid) -> Option<&Self::Out> {
        let sibling_pid = pid - 1;
        self.refresh_process(sibling_pid).then(|| ())?;
        self.process(sibling_pid)
    }
    fn find_sibling_process<F, T>(&mut self, pid: Pid, extract_args: F) -> Option<T>
    where
        F: Fn(&[String]) -> ProcessArgs<T>,
        Self: Sized,
    {
        self.refresh_processes();

        let this_start_time = self.process(pid)?.start_time();

        /*

        $ start_blame_of.sh src/main.rs | delta

        \_ /usr/bin/some-terminal-emulator
        |   \_ common_git_and_delta_ancestor
        |       \_ /bin/sh /opt/git/start_blame_of.sh src/main.rs
        |       |   \_ /bin/sh /opt/some/wrapper git blame src/main.rs
        |       |       \_ /usr/bin/git blame src/main.rs
        |       \_ /bin/sh /opt/some/wrapper delta
        |           \_ delta

        Walk up the process tree of delta and of every matching other process, counting the steps
        along the way.
        Find the common ancestor processes, calculate the distance, and select the one with the shortest.

        */

        let mut pid_distances = HashMap::<Pid, usize>::new();
        let mut collect_parent_pids = |pid, distance| {
            pid_distances.insert(pid, distance);
        };

        iter_parents(self, pid, &mut collect_parent_pids);

        let process_start_time_difference_less_than_3s = |a, b| (a as i64 - b as i64).abs() < 3;

        let cmdline_of_closest_matching_process = self
            .processes()
            .iter()
            .filter(|(_, proc)| {
                process_start_time_difference_less_than_3s(this_start_time, proc.start_time())
            })
            .filter_map(|(&pid, proc)| match extract_args(proc.cmd()) {
                ProcessArgs::Args(args) => {
                    let mut length_of_process_chain = usize::MAX;

                    let mut sum_distance = |pid, distance| {
                        if length_of_process_chain == usize::MAX {
                            if let Some(distance_to_first_common_parent) = pid_distances.get(&pid) {
                                length_of_process_chain =
                                    distance_to_first_common_parent + distance;
                            }
                        }
                    };
                    iter_parents(self, pid, &mut sum_distance);

                    Some((length_of_process_chain, args))
                }
                _ => None,
            })
            .min_by_key(|(distance, _)| *distance)
            .map(|(_, result)| result);

        cmdline_of_closest_matching_process
    }
}

impl ProcessInterface for ProcInfo {
    type Out = Process;

    fn my_pid(&self) -> Pid {
        std::process::id() as Pid
    }
    fn refresh_process(&mut self, pid: Pid) -> bool {
        self.info.refresh_process(pid)
    }
    fn process(&self, pid: Pid) -> Option<&Self::Out> {
        self.info.process(pid)
    }
    fn processes(&self) -> &HashMap<Pid, Self::Out> {
        self.info.processes()
    }
    fn refresh_processes(&mut self) {
        self.info.refresh_processes()
    }
}

fn calling_process_cmdline<P, F, T>(mut info: P, extract_args: F) -> Option<T>
where
    P: ProcessInterface,
    F: Fn(&[String]) -> ProcessArgs<T>,
{
    #[cfg(test)]
    {
        if let Some(args) = tests::FakeParentArgs::get() {
            match extract_args(&args) {
                ProcessArgs::Args(result) => return Some(result),
                _ => return None,
            }
        }
    }

    let my_pid = info.my_pid();

    // 1) Try the parent process. If delta is set as the pager in git, then git is the parent process.
    let parent = info.parent_process(my_pid)?;

    match extract_args(parent.cmd()) {
        ProcessArgs::Args(result) => return Some(result),
        ProcessArgs::ArgError => return None,

        // 2) The parent process was something else, this can happen if git output is piped into delta, e.g.
        // `git blame foo.txt | delta`. When the shell sets up the pipe it creates the two processes, the pids
        // are usually consecutive, so check if the process with `my_pid - 1` matches.
        ProcessArgs::OtherProcess => {
            let sibling = info.naive_sibling_process(my_pid);
            if let Some(proc) = sibling {
                if let ProcessArgs::Args(result) = extract_args(proc.cmd()) {
                    return Some(result);
                }
            }
            // else try the fallback
        }
    }

    /*
    3) Neither parent nor direct sibling were a match.
    The most likely case is that the input program of the pipe wrote all its data and exited before delta
    started, so no command line can be parsed. Same if the data was piped from an input file.

    There might also be intermediary scripts in between or piped input with a gap in pids or (rarely)
    randomized pids, so check all processes for the closest match in the process tree.

    100 /usr/bin/some-terminal-emulator
    124  \_ -shell
    301  |   \_ /usr/bin/git blame src/main.rs
    302  |       \_ wraps_delta.sh
    303  |           \_ delta
    304  |               \_ less --RAW-CONTROL-CHARS --quit-if-one-screen
    125  \_ -shell
    800  |   \_ /usr/bin/git blame src/main.rs
    200  |   \_ delta
    400  |       \_ less --RAW-CONTROL-CHARS --quit-if-one-screen
    126  \_ -shell
    501  |   \_ /bin/sh /wrapper/for/git blame src/main.rs
    555  |   |   \_ /usr/bin/git blame src/main.rs
    502  |   \_ delta
    567  |       \_ less --RAW-CONTROL-CHARS --quit-if-one-screen

    */
    info.find_sibling_process(my_pid, extract_args)
}

// Walk up the process tree, calling `f` with the pid and the distance to `starting_pid`.
// Prerequisite: `info.refresh_processes()` has been called.
fn iter_parents<P, F>(info: &P, starting_pid: Pid, f: F)
where
    P: ProcessInterface,
    F: FnMut(Pid, usize),
{
    fn inner_iter_parents<P, F>(info: &P, pid: Pid, mut f: F, distance: usize)
    where
        P: ProcessInterface,
        F: FnMut(Pid, usize),
    {
        // Probably bad input, not a tree:
        if distance > 2000 {
            return;
        }
        if let Some(proc) = info.process(pid) {
            if let Some(pid) = proc.parent() {
                f(pid, distance);
                inner_iter_parents(info, pid, f, distance + 1)
            }
        }
    }
    inner_iter_parents(info, starting_pid, f, 1)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    use itertools::Itertools;
    use std::cell::RefCell;
    use std::rc::Rc;

    thread_local! {
        static FAKE_ARGS: RefCell<TlsState<Vec<String>>> = RefCell::new(TlsState::None);
    }

    #[derive(Debug, PartialEq)]
    enum TlsState<T> {
        Once(T),
        Scope(T),
        With(usize, Rc<Vec<T>>),
        None,
        Invalid,
    }

    // When calling `FakeParentArgs::get()`, it can return `Some(values)` which were set earlier
    // during in the #[test]. Otherwise returns None.
    // This value can be valid once: `FakeParentArgs::once(val)`, for the entire scope:
    // `FakeParentArgs::for_scope(val)`, or can be different values everytime `get()` is called:
    // `FakeParentArgs::with([val1, val2, val3])`.
    // It is an error if `once` or `with` values remain unused, or are overused.
    // Note: The values are stored per-thread, so the expectation is that no thread boundaries are
    // crossed.
    pub struct FakeParentArgs {}
    impl FakeParentArgs {
        pub fn once(args: &str) -> Self {
            Self::new(args, |v| TlsState::Once(v), "once")
        }
        pub fn for_scope(args: &str) -> Self {
            Self::new(args, |v| TlsState::Scope(v), "for_scope")
        }
        fn new<F>(args: &str, initial: F, from_: &str) -> Self
        where
            F: Fn(Vec<String>) -> TlsState<Vec<String>>,
        {
            let string_vec = args.split(' ').map(str::to_owned).collect();
            if FAKE_ARGS.with(|a| a.replace(initial(string_vec))) != TlsState::None {
                Self::error(from_);
            }
            FakeParentArgs {}
        }
        pub fn with(args: &[&str]) -> Self {
            let with = TlsState::With(
                0,
                Rc::new(
                    args.iter()
                        .map(|a| a.split(' ').map(str::to_owned).collect())
                        .collect(),
                ),
            );
            if FAKE_ARGS.with(|a| a.replace(with)) != TlsState::None || args.is_empty() {
                Self::error("with creation");
            }
            FakeParentArgs {}
        }
        pub fn get() -> Option<Vec<String>> {
            FAKE_ARGS.with(|a| {
                let old_value = a.replace_with(|old_value| match old_value {
                    TlsState::Once(_) => TlsState::Invalid,
                    TlsState::Scope(args) => TlsState::Scope(args.clone()),
                    TlsState::With(n, args) => TlsState::With(*n + 1, Rc::clone(args)),
                    TlsState::None => TlsState::None,
                    TlsState::Invalid => TlsState::Invalid,
                });

                match old_value {
                    TlsState::Once(args) | TlsState::Scope(args) => Some(args),
                    TlsState::With(n, args) if n < args.len() => Some(args[n].clone()),
                    TlsState::None => None,
                    TlsState::Invalid | TlsState::With(_, _) => Self::error("get"),
                }
            })
        }
        fn error(where_: &str) -> ! {
            panic!(
                "test logic error (in {}): wrong FakeParentArgs scope?",
                where_
            );
        }
    }
    impl Drop for FakeParentArgs {
        fn drop(&mut self) {
            // Clears an Invalid state and tests if a Once or With value has been used.
            FAKE_ARGS.with(|a| {
                let old_value = a.replace(TlsState::None);
                match old_value {
                    TlsState::With(n, args) => {
                        if n != args.len() {
                            Self::error("drop with")
                        }
                    }
                    TlsState::Once(_) | TlsState::None => Self::error("drop"),
                    TlsState::Scope(_) | TlsState::Invalid => {}
                }
            });
        }
    }

    #[test]
    fn test_guess_git_blame_filename_extension() {
        use ProcessArgs::Args;

        fn make_string_vec(args: &[&str]) -> Vec<String> {
            args.iter().map(|&x| x.to_owned()).collect::<Vec<String>>()
        }
        let args = make_string_vec(&["git", "blame", "hello", "world.txt"]);
        assert_eq!(
            guess_git_blame_filename_extension(&args),
            Args("txt".into())
        );

        let args = make_string_vec(&[
            "git",
            "blame",
            "-s",
            "-f",
            "hello.txt",
            "--date=2015",
            "--date",
            "now",
        ]);
        assert_eq!(
            guess_git_blame_filename_extension(&args),
            Args("txt".into())
        );

        let args = make_string_vec(&["git", "blame", "-s", "-f", "--", "hello.txt"]);
        assert_eq!(
            guess_git_blame_filename_extension(&args),
            Args("txt".into())
        );

        let args = make_string_vec(&["git", "blame", "--", "--not.an.argument"]);
        assert_eq!(
            guess_git_blame_filename_extension(&args),
            Args("argument".into())
        );

        let args = make_string_vec(&["foo", "bar", "-a", "--123", "not.git"]);
        assert_eq!(
            guess_git_blame_filename_extension(&args),
            ProcessArgs::OtherProcess
        );

        let args = make_string_vec(&["git", "blame", "--help.txt"]);
        assert_eq!(
            guess_git_blame_filename_extension(&args),
            ProcessArgs::ArgError
        );

        let args = make_string_vec(&["git", "-c", "a=b", "blame", "main.rs"]);
        assert_eq!(guess_git_blame_filename_extension(&args), Args("rs".into()));

        let args = make_string_vec(&["git", "blame", "README"]);
        assert_eq!(
            guess_git_blame_filename_extension(&args),
            Args("README".into())
        );

        let args = make_string_vec(&["git", "blame", ""]);
        assert_eq!(guess_git_blame_filename_extension(&args), Args("".into()));
    }

    #[derive(Debug, Default)]
    struct FakeProc {
        pid: Pid,
        start_time: u64,
        cmd: Vec<String>,
        ppid: Option<Pid>,
    }
    impl FakeProc {
        fn new(pid: Pid, start_time: u64, cmd: Vec<String>, ppid: Option<Pid>) -> Self {
            FakeProc {
                pid,
                start_time,
                cmd,
                ppid,
            }
        }
    }

    impl ProcActions for FakeProc {
        fn cmd(&self) -> &[String] {
            &self.cmd
        }
        fn parent(&self) -> Option<Pid> {
            self.ppid
        }
        fn start_time(&self) -> u64 {
            self.start_time
        }
    }

    #[derive(Debug, Default)]
    struct MockProcInfo {
        delta_pid: Pid,
        info: HashMap<Pid, FakeProc>,
    }
    impl MockProcInfo {
        fn with(processes: &[(Pid, u64, &str, Option<Pid>)]) -> Self {
            MockProcInfo {
                delta_pid: processes.last().map(|p| p.0).unwrap_or(1),
                info: processes
                    .into_iter()
                    .map(|(pid, start_time, cmd, ppid)| {
                        let cmd_vec = cmd.split(' ').map(str::to_owned).collect();
                        (*pid, FakeProc::new(*pid, *start_time, cmd_vec, *ppid))
                    })
                    .collect(),
            }
        }
    }

    impl ProcessInterface for MockProcInfo {
        type Out = FakeProc;

        fn my_pid(&self) -> Pid {
            self.delta_pid
        }
        fn process(&self, pid: Pid) -> Option<&Self::Out> {
            self.info.get(&pid)
        }
        fn processes(&self) -> &HashMap<Pid, Self::Out> {
            &self.info
        }
        fn refresh_processes(&mut self) {}
        fn refresh_process(&mut self, _pid: Pid) -> bool {
            true
        }
    }

    #[test]
    fn test_process_testing() {
        {
            let _args = FakeParentArgs::once(&"git blame hello");
            assert_eq!(
                calling_process_cmdline(ProcInfo::new(), guess_git_blame_filename_extension),
                Some("hello".into())
            );
        }
        {
            let _args = FakeParentArgs::once(&"git blame world.txt");
            assert_eq!(
                calling_process_cmdline(ProcInfo::new(), guess_git_blame_filename_extension),
                Some("txt".into())
            );
        }
        {
            let _args = FakeParentArgs::for_scope(&"git blame hello world.txt");
            assert_eq!(
                calling_process_cmdline(ProcInfo::new(), guess_git_blame_filename_extension),
                Some("txt".into())
            );

            assert_eq!(
                calling_process_cmdline(ProcInfo::new(), guess_git_blame_filename_extension),
                Some("txt".into())
            );
        }
    }

    #[test]
    #[should_panic]
    fn test_process_testing_assert() {
        let _args = FakeParentArgs::once(&"git blame do.not.panic");
        assert_eq!(
            calling_process_cmdline(ProcInfo::new(), guess_git_blame_filename_extension),
            Some("panic".into())
        );

        calling_process_cmdline(ProcInfo::new(), guess_git_blame_filename_extension);
    }

    #[test]
    #[should_panic]
    fn test_process_testing_assert_never_used() {
        let _args = FakeParentArgs::once(&"never used");

        // causes a panic while panicing, so can't test:
        // let _args = FakeParentArgs::for_scope(&"never used");
        // let _args = FakeParentArgs::once(&"never used");
    }

    #[test]
    fn test_process_testing_scope_can_remain_unused() {
        let _args = FakeParentArgs::for_scope(&"never used");
    }

    #[test]
    fn test_process_testing_n_times_panic() {
        let _args = FakeParentArgs::with(&["git blame once", "git blame twice"]);
        assert_eq!(
            calling_process_cmdline(ProcInfo::new(), guess_git_blame_filename_extension),
            Some("once".into())
        );

        assert_eq!(
            calling_process_cmdline(ProcInfo::new(), guess_git_blame_filename_extension),
            Some("twice".into())
        );
    }

    #[test]
    #[should_panic]
    fn test_process_testing_n_times_unused() {
        let _args = FakeParentArgs::with(&["git blame once", "git blame twice"]);
    }

    #[test]
    #[should_panic]
    fn test_process_testing_n_times_underused() {
        let _args = FakeParentArgs::with(&["git blame once", "git blame twice"]);
        assert_eq!(
            calling_process_cmdline(ProcInfo::new(), guess_git_blame_filename_extension),
            Some("once".into())
        );
    }

    #[test]
    #[should_panic]
    #[ignore]
    fn test_process_testing_n_times_overused() {
        let _args = FakeParentArgs::with(&["git blame once"]);
        assert_eq!(
            calling_process_cmdline(ProcInfo::new(), guess_git_blame_filename_extension),
            Some("once".into())
        );
        // ignored: dropping causes a panic while panicing, so can't test
        calling_process_cmdline(ProcInfo::new(), guess_git_blame_filename_extension);
    }

    #[test]
    fn test_process_blame_info_with_parent() {
        let no_processes = MockProcInfo::with(&[]);
        assert_eq!(
            calling_process_cmdline(no_processes, guess_git_blame_filename_extension),
            None
        );

        let parent = MockProcInfo::with(&[
            (2, 100, "-shell", None),
            (3, 100, "git blame hello.txt", Some(2)),
            (4, 100, "delta", Some(3)),
        ]);
        assert_eq!(
            calling_process_cmdline(parent, guess_git_blame_filename_extension),
            Some("txt".into())
        );

        let grandparent = MockProcInfo::with(&[
            (2, 100, "-shell", None),
            (3, 100, "git blame src/main.rs", Some(2)),
            (4, 100, "call_delta.sh", Some(3)),
            (5, 100, "delta", Some(4)),
        ]);
        assert_eq!(
            calling_process_cmdline(grandparent, guess_git_blame_filename_extension),
            Some("rs".into())
        );
    }

    #[test]
    fn test_describe_calling_process_grep() {
        let no_processes = MockProcInfo::with(&[]);
        assert_eq!(
            calling_process_cmdline(no_processes, describe_calling_process),
            None
        );

        let parent = MockProcInfo::with(&[
            (2, 100, "-shell", None),
            (3, 100, "git grep pattern hello.txt", Some(2)),
            (4, 100, "delta", Some(3)),
        ]);
        assert_eq!(
            calling_process_cmdline(parent, describe_calling_process),
            Some(CallingProcess::GitGrep(([].into(), [].into())))
        );

        let parent = MockProcInfo::with(&[
            (2, 100, "-shell", None),
            (3, 100, "Git.exe grep pattern hello.txt", Some(2)),
            (4, 100, "delta", Some(3)),
        ]);
        assert_eq!(
            calling_process_cmdline(parent, describe_calling_process),
            Some(CallingProcess::GitGrep(([].into(), [].into())))
        );

        for grep_command in &[
            "/usr/local/bin/rg pattern hello.txt",
            "RG.exe pattern hello.txt",
            "/usr/local/bin/ack pattern hello.txt",
            "ack.exe pattern hello.txt",
        ] {
            let parent = MockProcInfo::with(&[
                (2, 100, "-shell", None),
                (3, 100, grep_command, Some(2)),
                (4, 100, "delta", Some(3)),
            ]);
            assert_eq!(
                calling_process_cmdline(parent, describe_calling_process),
                Some(CallingProcess::OtherGrep)
            );
        }

        fn set(arg1: &[&str]) -> HashSet<String> {
            arg1.iter().map(|&s| s.to_owned()).collect()
        }

        let git_grep_command =
            "git grep -ab --function-context -n --show-function -W --foo=val pattern hello.txt";

        let expected_result = Some(CallingProcess::GitGrep((
            set(&["--function-context", "--show-function", "--foo"]),
            set(&["-a", "-b", "-n", "-W"]),
        )));

        let parent = MockProcInfo::with(&[
            (2, 100, "-shell", None),
            (3, 100, git_grep_command, Some(2)),
            (4, 100, "delta", Some(3)),
        ]);
        assert_eq!(
            calling_process_cmdline(parent, describe_calling_process),
            expected_result
        );

        let grandparent = MockProcInfo::with(&[
            (2, 100, "-shell", None),
            (3, 100, git_grep_command, Some(2)),
            (4, 100, "call_delta.sh", Some(3)),
            (5, 100, "delta", Some(4)),
        ]);
        assert_eq!(
            calling_process_cmdline(grandparent, describe_calling_process),
            expected_result
        );
    }

    #[test]
    fn test_describe_calling_process_git_show() {
        for (command, expected_extension) in [
            ("/usr/local/bin/git show 775c3b84:./src/hello.rs", "rs"),
            ("/usr/local/bin/git show HEAD~1:Makefile", "Makefile"),
            (
                "git -c x.y=z show --abbrev-commit 775c3b84:./src/hello.bye.R",
                "R",
            ),
        ] {
            let parent = MockProcInfo::with(&[
                (2, 100, "-shell", None),
                (3, 100, command, Some(2)),
                (4, 100, "delta", Some(3)),
            ]);
            assert_eq!(
                calling_process_cmdline(parent, describe_calling_process),
                Some(CallingProcess::GitShow(expected_extension.to_string())),
            );
        }
    }

    #[test]
    fn test_process_calling_cmdline() {
        // Github runs CI tests for arm under qemu where where sysinfo can not find the parent process.
        if std::env::vars().any(|(key, _)| key == "CROSS_RUNNER" || key == "QEMU_LD_PREFIX") {
            return;
        }

        let mut info = ProcInfo::new();
        info.refresh_processes();
        let mut ppid_distance = Vec::new();

        iter_parents(&info, std::process::id() as Pid, |pid, distance| {
            ppid_distance.push(pid as i32);
            ppid_distance.push(distance as i32)
        });

        assert!(ppid_distance[1] == 1);

        fn find_calling_process(args: &[String], want: &[&str]) -> ProcessArgs<()> {
            if args.iter().any(|have| want.iter().any(|want| want == have)) {
                ProcessArgs::Args(())
            } else {
                ProcessArgs::ArgError
            }
        }

        // Tests that caller is something like "cargo test" or "cargo tarpaulin"
        let find_test = |args: &[String]| find_calling_process(args, &["test", "tarpaulin"]);
        assert_eq!(calling_process_cmdline(info, find_test), Some(()));

        let nonsense = ppid_distance
            .iter()
            .map(|i| i.to_string())
            .join("Y40ii4RihK6lHiK4BDsGSx");

        let find_nothing = |args: &[String]| find_calling_process(args, &[&nonsense]);
        assert_eq!(calling_process_cmdline(ProcInfo::new(), find_nothing), None);
    }
}
