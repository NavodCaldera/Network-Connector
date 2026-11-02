import 'rust/api.dart' as rust;

export 'rust/api.dart'
    show
        ActivityKind,
        AgentStart,
        AppSettings,
        EnrolRequest,
        Enrolment,
        EnrolmentCheck,
        EnrolmentState,
        FetchedOutput,
        JobPhase,
        HelperActivity,
        HelperState,
        HelperStatus,
        LoadedSettings,
        NetworkKind,
        NightDataSettings,
        NightDataStatus,
        PolicySummary,
        RequestedJob,
        SubmitJobRequest,
        TariffPreset,
        TimeWindow,
        TodaySummary;

/// The Rust calls the UI needs. Mirrors `nc-bridge` 1:1 so widget tests can use a fake.
/// All validation and tariff maths happen in Rust; the UI only shows the results.
abstract class AgentApi {
  /// Creates the agent: coordinator-backed when `dir` holds an enrolment. Call before anything else.
  rust.AgentStart openAgent(String dir);

  /// Registers this device with the coordinator. Throws a message string on failure.
  Future<rust.Enrolment> enrol(String dir, rust.EnrolRequest request);

  /// Asks the coordinator (signed, network) whether it still knows this device.
  Future<rust.EnrolmentCheck> checkEnrolment(String dir);

  /// Stops helping and tells the coordinator to forget this device. Throws a message string when the
  /// coordinator could not be told; a local reset is still allowed.
  Future<void> deregisterDevice(String dir);

  /// Stops helping and removes the enrolment and device identity.
  Future<void> unenrol(String dir);

  rust.AppSettings defaultSettings();
  Future<rust.LoadedSettings> loadSettings(String dir);
  Future<void> saveSettings(String dir, rust.AppSettings settings);

  /// `null` when valid, otherwise a plain-English message from nc-policy.
  String? validatePolicy(rust.PolicySummary policy);

  /// `null` when valid, otherwise a plain-English message from nc-tariff.
  String? validateNightData(rust.NightDataSettings nightData);
  List<rust.TariffPreset> tariffPresets();

  /// Throws a message string when the settings are invalid.
  rust.NightDataStatus nightDataStatus(rust.NightDataSettings nightData);
  rust.TodaySummary todaySummary();
  rust.HelperActivity helperActivity();
  String coreVersion();

  Future<void> startHelperSession(rust.PolicySummary policy);
  Future<void> stopHelperSession();

  /// Applies new limits to the running session at once. Throws a message string when invalid.
  Future<void> updatePolicy(rust.PolicySummary policy);

  /// Applies the night-data budget to the agent at once. Throws a message string when invalid.
  Future<void> updateNightData(rust.NightDataSettings nightData);
  Stream<rust.HelperStatus> watchStatus();

  /// Requester: signs the job with this device's key, records it and sends it. Paths only, never bytes.
  /// Throws a message string when the request is invalid.
  Future<rust.RequestedJob> submitJob(
    String dir,
    rust.SubmitJobRequest request,
  );

  /// Requester: jobs recorded on this device, newest first (local files only).
  Future<List<rust.RequestedJob>> listJobs(String dir);

  /// Requester: asks the coordinator about one job. Throws a message string.
  Future<rust.RequestedJob> jobStatus(String dir, String jobId);

  /// Requester: downloads, hash-checks and saves the output. Throws a message string.
  Future<rust.FetchedOutput> fetchOutput(
    String dir,
    String jobId,
    String outPath,
  );
}

/// The real agent, in-process through flutter_rust_bridge.
class RustAgentApi implements AgentApi {
  @override
  rust.AgentStart openAgent(String dir) => rust.openAgent(dir: dir);

  @override
  Future<rust.Enrolment> enrol(String dir, rust.EnrolRequest request) =>
      rust.enrol(dir: dir, request: request);

  @override
  Future<rust.EnrolmentCheck> checkEnrolment(String dir) =>
      rust.checkEnrolment(dir: dir);

  @override
  Future<void> deregisterDevice(String dir) => rust.deregisterDevice(dir: dir);

  @override
  Future<void> unenrol(String dir) => rust.unenrol(dir: dir);

  @override
  rust.AppSettings defaultSettings() => rust.defaultSettings();

  @override
  Future<rust.LoadedSettings> loadSettings(String dir) =>
      rust.loadSettings(dir: dir);

  @override
  Future<void> saveSettings(String dir, rust.AppSettings settings) =>
      rust.saveSettings(dir: dir, settings: settings);

  @override
  String? validatePolicy(rust.PolicySummary policy) =>
      rust.validatePolicy(policy: policy);

  @override
  String? validateNightData(rust.NightDataSettings nightData) =>
      rust.validateNightData(nightData: nightData);

  @override
  List<rust.TariffPreset> tariffPresets() => rust.tariffPresets();

  @override
  rust.NightDataStatus nightDataStatus(rust.NightDataSettings nightData) =>
      rust.nightDataStatus(nightData: nightData);

  @override
  rust.TodaySummary todaySummary() => rust.todaySummary();

  @override
  rust.HelperActivity helperActivity() => rust.helperActivity();

  @override
  String coreVersion() => rust.coreVersion();

  @override
  Future<void> updatePolicy(rust.PolicySummary policy) =>
      rust.updatePolicy(policy: policy);

  @override
  Future<void> updateNightData(rust.NightDataSettings nightData) =>
      rust.updateNightData(nightData: nightData);

  @override
  Future<void> startHelperSession(rust.PolicySummary policy) =>
      rust.startHelperSession(policy: policy);

  @override
  Future<void> stopHelperSession() => rust.stopHelperSession();

  @override
  Stream<rust.HelperStatus> watchStatus() => rust.watchStatus();

  @override
  Future<rust.RequestedJob> submitJob(
    String dir,
    rust.SubmitJobRequest request,
  ) => rust.submitJob(dir: dir, request: request);

  @override
  Future<List<rust.RequestedJob>> listJobs(String dir) =>
      rust.listJobs(dir: dir);

  @override
  Future<rust.RequestedJob> jobStatus(String dir, String jobId) =>
      rust.jobStatus(dir: dir, jobId: jobId);

  @override
  Future<rust.FetchedOutput> fetchOutput(
    String dir,
    String jobId,
    String outPath,
  ) => rust.fetchOutput(dir: dir, jobId: jobId, outPath: outPath);
}
