import 'rust/api.dart' as rust;

export 'rust/api.dart'
    show
        ActivityKind,
        AppSettings,
        HelperActivity,
        HelperState,
        HelperStatus,
        LoadedSettings,
        NightDataSettings,
        NightDataStatus,
        PolicySummary,
        TariffPreset,
        TimeWindow,
        TodaySummary;

/// The Rust calls the UI needs. Mirrors `nc-bridge` 1:1 so widget tests can use a fake.
/// All validation and tariff maths happen in Rust; the UI only shows the results.
abstract class AgentApi {
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
}

/// The real agent, in-process through flutter_rust_bridge.
class RustAgentApi implements AgentApi {
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
}
