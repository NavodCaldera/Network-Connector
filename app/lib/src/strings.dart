/// All UI text in one place, so Sinhala and Tamil translations can be added later
/// (move to `flutter gen-l10n` ARB files when the first translation arrives).
class S {
  static const appTitle = 'Network Connector';

  // Navigation
  static const navHome = 'Home';
  static const navLimits = 'Limits';
  static const navNight = 'Night data';
  static const navAbout = 'About';

  // Home
  static const giveRole = 'Give performance';
  static const askRole = 'Ask for performance';
  static const askSoon = 'Coming later';
  static const yourRules = 'Your rules';
  static const acceptJobs = 'Accept jobs';
  static const acceptJobsHelp =
      'Off: this device helps no one, even while a session runs.';
  static const today = 'Today';
  static String helpedFor(int minutes) => 'Helped for $minutes min';
  static String jobsDone(int n) => n == 1 ? '1 job done' : '$n jobs done';
  static const stop = 'Stop';
  static const stopHelp = 'Stops all work on this device at once.';

  static String cpu(int p) => 'Uses at most $p% of the processor';
  static String limits(BigInt memMb, BigInt diskMb) =>
      'At most $memMb MB memory and $diskMb MB disk';
  static String charging(bool only, int battery) => only
      ? 'Only while charging, battery at least $battery%'
      : 'Battery at least $battery%';
  static String maxTask(BigInt minutes) => 'Each job at most $minutes minutes';
  static String pauseWhenUsed(bool on) =>
      on ? 'Pauses when you use the device' : 'Keeps working while you use it';
  static const noHours = 'No working hours set, so no work runs yet';
  static String hoursSet(int n) =>
      n == 1 ? 'Works only in your set hours' : 'Works only in $n time windows';

  static const stateStopped = 'Stopped. This device is not helping.';
  static const stateStarting = 'Starting...';
  static const stateWaiting = 'Helping. Waiting for jobs.';
  static const stateNotAccepting = 'Running, but not accepting jobs.';
  static const stateStopping = 'Stopping...';
  static String stoppedIn(int ms) => 'Stopped in $ms ms.';

  // Limits
  static const limitsTitle = 'Your limits';
  static String cpuLabel(int p) => 'Processor: at most $p%';
  static const cpuHelp = 'Best effort. The system may not enforce it exactly.';
  static const memoryLabel = 'Memory for jobs (MB)';
  static const diskLabel = 'Disk for jobs (MB)';
  static const maxTaskLabel = 'Longest job (minutes)';
  static const chargingOnly = 'Only when charging';
  static String batteryLabel(int p) => 'Battery at least $p%';
  static const idleOnly = 'Only when idle';
  static const idleMinutesLabel = 'Idle for at least (minutes)';
  static const hoursTitle = 'Allowed hours';
  static const hoursOn = 'Only at set hours';
  static const hoursOff =
      'Off: no hours are set, so jobs never run. Turn on to choose hours.';
  static const timezoneLabel = 'Time zone';
  static const save = 'Save';
  static const saved = 'Saved.';
  static const from = 'From';
  static const to = 'To';
  static const days = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];

  // Night data
  static const nightTitle = 'Night data';
  static const nightHelp =
      'Large downloads and uploads wait for your free night window. '
      'Check the hours against your own data package.';
  static String presetLabel(String id) => switch (id) {
    'night-00-08' => 'Midnight to 8 am',
    'night-00-06' => 'Midnight to 6 am',
    'night-22-06' => '10 pm to 6 am',
    'custom' => 'My own hours',
    _ => id,
  };
  static const budgetLabel = 'Most data per night (MB)';
  static const neverPaid = 'Never use paid data';
  static const neverPaidHelp =
      'On: nothing is sent outside the free window, not even status.';
  static const paidAllowedHelp =
      'Off: only small status messages may use paid data.';
  static const openNow = 'Free window open now';
  static String openUntil(String end) => 'Open until $end';
  static String nextWindow(String start, String end) =>
      'Next free window: $start to $end';
  static const noWindow = 'No free window found.';
  static String budgetLeft(String left, String total) =>
      'Data left in this window: $left of $total';
  static const budgetEstimate =
      'An estimate from this app, not your provider\'s bill.';
  static String mb(BigInt bytes) =>
      '${(bytes ~/ BigInt.from(1000000)).toString()} MB';

  // About
  static const aboutTitle = 'About';
  static String appVersion(String v) => 'App version $v';
  static String coreVersion(String v) => 'Core version $v';
  static const licences = 'Licences';
  static const aboutText =
      'Lets you share spare computer time with people you trust. '
      'You stay in control: Stop always works.';
}
