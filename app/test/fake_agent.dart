import 'dart:async';
import 'dart:typed_data';

import 'package:network_connector/src/agent_api.dart';

/// In-memory agent with the same state sequence as nc-agent. Validation mimics two nc-policy /
/// nc-tariff messages only; the real rules are tested in Rust (nc-bridge tests).
class FakeAgent implements AgentApi {
  FakeAgent({this.stored, this.notice});

  final _ctl = StreamController<HelperStatus>.broadcast();
  var _seq = 0;
  final started = <PolicySummary>[];
  final saves = <AppSettings>[];
  var stops = 0;
  AppSettings? stored;
  String? notice;
  BigInt helpingSeconds = BigInt.zero;

  void _emit(HelperState s) =>
      _ctl.add(HelperStatus(state: s, seq: BigInt.from(++_seq)));

  static TimeWindow window(int start, int end) => TimeWindow(
    days: Uint8List.fromList([0, 1, 2, 3, 4, 5, 6]),
    startMinute: start,
    endMinute: end,
  );

  @override
  AppSettings defaultSettings() => AppSettings(
    policy: PolicySummary(
      acceptJobs: false,
      cpuBudgetPercent: 25,
      maxMemoryMb: BigInt.from(256),
      maxDiskMb: BigInt.from(256),
      chargingOnly: true,
      minBatteryPercent: 20,
      maxTaskMinutes: BigInt.from(5),
      pauseOnUserActivity: true,
      idleMinutes: 5,
      timezone: 'Asia/Colombo',
      hours: const [],
    ),
    nightData: NightDataSettings(
      presetId: 'night-00-08',
      window: window(0, 480),
      nightlyBudgetMb: BigInt.from(1000),
      neverPaidData: true,
      timezone: 'Asia/Colombo',
    ),
  );

  @override
  Future<LoadedSettings> loadSettings(String dir) async => LoadedSettings(
    settings: stored ?? defaultSettings(),
    resetNotice: notice,
  );

  @override
  Future<void> saveSettings(String dir, AppSettings settings) async {
    saves.add(settings);
    stored = settings;
  }

  @override
  String? validatePolicy(PolicySummary p) {
    if (p.cpuBudgetPercent == 0) {
      return 'CPU budget must be between 1 and 100 percent.';
    }
    if (p.maxMemoryMb == BigInt.zero) {
      return 'Memory limit must be greater than zero.';
    }
    return null;
  }

  @override
  String? validateNightData(NightDataSettings n) =>
      n.nightlyBudgetMb == BigInt.zero
      ? 'Nightly data budget must be more than 0 MB.'
      : null;

  @override
  List<TariffPreset> tariffPresets() => [
    TariffPreset(id: 'night-00-08', window: window(0, 480)),
    TariffPreset(id: 'night-00-06', window: window(0, 360)),
    TariffPreset(id: 'night-22-06', window: window(1320, 360)),
  ];

  final statusCalls = <NightDataSettings>[];

  @override
  NightDataStatus nightDataStatus(NightDataSettings n) {
    statusCalls.add(n);
    final total = n.nightlyBudgetMb * BigInt.from(1000000);
    return NightDataStatus(
      openNow: false,
      windowStart: 'Tue 00:00',
      windowEnd: 'Tue 07:59',
      budgetLeftBytes: total,
      budgetTotalBytes: total,
    );
  }

  @override
  TodaySummary todaySummary() =>
      TodaySummary(helpingSeconds: helpingSeconds, jobsDone: BigInt.zero);

  @override
  String coreVersion() => '0.1.0';

  @override
  Future<void> startHelperSession(PolicySummary policy) async {
    started.add(policy);
    _emit(HelperState.starting);
    _emit(
      policy.acceptJobs ? HelperState.waitingForJobs : HelperState.notAccepting,
    );
  }

  @override
  Future<void> stopHelperSession() async {
    stops++;
    _emit(HelperState.stopping);
    _emit(HelperState.stopped);
  }

  @override
  Stream<HelperStatus> watchStatus() => _ctl.stream;
}
