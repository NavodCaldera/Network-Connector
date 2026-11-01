/// All UI text in one place, so Sinhala and Tamil translations can be added later
/// (move to `flutter gen-l10n` ARB files when the first translation arrives).
class S {
  static const appTitle = 'Network Connector';
  static const giveRole = 'Give performance';
  static const askRole = 'Ask for performance';
  static const askSoon = 'Coming later';
  static const yourRules = 'Your rules';
  static const acceptJobs = 'Accept jobs';
  static const acceptJobsHelp =
      'Off: this device helps no one, even while a session runs.';
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

  static const stateStopped = 'Stopped. This device is not helping.';
  static const stateStarting = 'Starting...';
  static const stateWaiting = 'Helping. Waiting for jobs.';
  static const stateNotAccepting = 'Running, but not accepting jobs.';
  static const stateStopping = 'Stopping...';
  static String stoppedIn(int ms) => 'Stopped in $ms ms.';
}
