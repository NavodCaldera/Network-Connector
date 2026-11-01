import 'dart:async';

import 'package:flutter/foundation.dart';

import 'agent_api.dart';

/// App state shared by every screen: saved settings and the live helper status.
/// Validation and saving are done by Rust; this class only calls it and stores the result.
class AppModel extends ChangeNotifier {
  AppModel(this.api, this.settingsDir) : settings = api.defaultSettings();

  final AgentApi api;

  /// App-support folder for `settings.json`; null when the platform has none (settings are not saved).
  final String? settingsDir;

  AppSettings settings;
  HelperState state = HelperState.stopped;
  String? error;
  String? resetNotice;
  int? lastStopMs;
  StreamSubscription<HelperStatus>? _sub;
  Stopwatch? _stopClock;

  PolicySummary get policy => settings.policy;
  bool get running =>
      state != HelperState.stopped && state != HelperState.stopping;

  /// Subscribe to status first, so Stop works even while settings load.
  Future<void> start() async {
    _sub = api.watchStatus().listen(_onStatus);
    final dir = settingsDir;
    if (dir == null) return;
    final loaded = await api.loadSettings(dir);
    settings = loaded.settings;
    resetNotice = loaded.resetNotice;
    notifyListeners();
  }

  @override
  void dispose() {
    _sub?.cancel();
    super.dispose();
  }

  void _onStatus(HelperStatus s) {
    state = s.state;
    final clock = _stopClock;
    if (s.state == HelperState.stopped && clock != null) {
      lastStopMs = clock.elapsedMilliseconds;
      _stopClock = null;
      debugPrint('stop latency: tap to Stopped = $lastStopMs ms');
    }
    notifyListeners();
  }

  Future<void> give() async {
    error = null;
    lastStopMs = null;
    notifyListeners();
    try {
      await api.startHelperSession(policy);
    } catch (e) {
      error = e.toString();
      notifyListeners();
    }
  }

  /// Never waits on anything else; Stop must always work.
  Future<void> stop() async {
    _stopClock = Stopwatch()..start();
    await api.stopHelperSession();
  }

  /// Validates in Rust, saves, and applies to a running session at once. Returns the error or null.
  Future<String?> savePolicy(PolicySummary p) async {
    final invalid = api.validatePolicy(p);
    if (invalid != null) return invalid;
    final next = AppSettings(policy: p, nightData: settings.nightData);
    final failed = await _persist(next);
    if (failed != null) return failed;
    // Changed limits (including withdrawn consent) take effect now, not at the next start.
    if (running) {
      await api.stopHelperSession();
      try {
        await api.startHelperSession(p);
      } catch (e) {
        error = e.toString();
      }
    }
    notifyListeners();
    return null;
  }

  Future<String?> saveNightData(NightDataSettings n) async {
    final invalid = api.validateNightData(n);
    if (invalid != null) return invalid;
    final failed = await _persist(
      AppSettings(policy: settings.policy, nightData: n),
    );
    notifyListeners();
    return failed;
  }

  Future<void> setAccept(bool on) async {
    final err = await savePolicy(copyPolicy(policy, acceptJobs: on));
    if (err != null) {
      error = err;
      notifyListeners();
    }
  }

  Future<String?> _persist(AppSettings next) async {
    final dir = settingsDir;
    if (dir != null) {
      try {
        await api.saveSettings(dir, next);
      } catch (e) {
        return e.toString();
      }
    }
    settings = next;
    return null;
  }
}

PolicySummary copyPolicy(
  PolicySummary p, {
  bool? acceptJobs,
  int? cpuBudgetPercent,
  BigInt? maxMemoryMb,
  BigInt? maxDiskMb,
  bool? chargingOnly,
  int? minBatteryPercent,
  BigInt? maxTaskMinutes,
  bool? pauseOnUserActivity,
  int? idleMinutes,
  String? timezone,
  List<TimeWindow>? hours,
}) => PolicySummary(
  acceptJobs: acceptJobs ?? p.acceptJobs,
  cpuBudgetPercent: cpuBudgetPercent ?? p.cpuBudgetPercent,
  maxMemoryMb: maxMemoryMb ?? p.maxMemoryMb,
  maxDiskMb: maxDiskMb ?? p.maxDiskMb,
  chargingOnly: chargingOnly ?? p.chargingOnly,
  minBatteryPercent: minBatteryPercent ?? p.minBatteryPercent,
  maxTaskMinutes: maxTaskMinutes ?? p.maxTaskMinutes,
  pauseOnUserActivity: pauseOnUserActivity ?? p.pauseOnUserActivity,
  idleMinutes: idleMinutes ?? p.idleMinutes,
  timezone: timezone ?? p.timezone,
  hours: hours ?? p.hours,
);
