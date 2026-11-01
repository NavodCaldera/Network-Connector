// Real Dart -> nc-bridge -> nc-agent round trip, on the host VM.
// Runs only when NC_BRIDGE_DLL points at a built library (e.g. C:\dev\nc-target\release\nc_bridge.dll);
// CI has no Rust build in the Flutter job, so it is skipped there.
import 'dart:async';
import 'dart:io';

import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart'
    show ExternalLibrary;
import 'package:flutter_test/flutter_test.dart';
import 'package:network_connector/src/rust/api.dart';
import 'package:network_connector/src/rust/frb_generated.dart';

void main() {
  final path = Platform.environment['NC_BRIDGE_DLL'];
  final skip = (path == null || !File(path).existsSync())
      ? 'set NC_BRIDGE_DLL to a built nc_bridge library'
      : null;

  test('start and stop a helper session through the real agent', () async {
    await RustLib.init(externalLibrary: ExternalLibrary.open(path!));
    final policy = defaultPolicy();
    expect(policy.acceptJobs, isFalse);

    final events = <HelperState>[];
    final sub = watchStatus().listen((s) => events.add(s.state));
    Future<void> until(HelperState s) async {
      final deadline = DateTime.now().add(const Duration(seconds: 5));
      while (!events.contains(s)) {
        if (DateTime.now().isAfter(deadline)) fail('no $s, got $events');
        await Future<void>.delayed(const Duration(milliseconds: 1));
      }
    }

    await startHelperSession(policy: policy);
    await until(HelperState.notAccepting);

    final clock = Stopwatch()..start();
    events.clear();
    await stopHelperSession();
    await until(HelperState.stopped);
    clock.stop();
    // ignore: avoid_print
    print('real bridge stop latency: ${clock.elapsedMicroseconds} us');
    expect(clock.elapsed, lessThan(const Duration(seconds: 1)));

    // frb completes cancel() only when Rust drops the sink, i.e. at the next status change; do not wait.
    unawaited(sub.cancel());

    // Settings, validation and tariff queries through the real Rust code.
    final bad = PolicySummary(
      acceptJobs: false,
      cpuBudgetPercent: 0,
      maxMemoryMb: policy.maxMemoryMb,
      maxDiskMb: policy.maxDiskMb,
      chargingOnly: true,
      minBatteryPercent: 20,
      maxTaskMinutes: policy.maxTaskMinutes,
      pauseOnUserActivity: true,
      idleMinutes: 5,
      timezone: 'Asia/Colombo',
      hours: const [],
    );
    expect(
      validatePolicy(policy: bad),
      'CPU budget must be between 1 and 100 percent.',
    );
    final defaults = defaultSettings();
    expect(validateNightData(nightData: defaults.nightData), isNull);
    final night = nightDataStatus(nightData: defaults.nightData);
    expect(night.budgetTotalBytes, BigInt.from(1000000000));

    final dir = await Directory.systemTemp.createTemp('nc-settings-');
    try {
      expect((await loadSettings(dir: dir.path)).resetNotice, isNull);
      await saveSettings(dir: dir.path, settings: defaults);
      final loaded = await loadSettings(dir: dir.path);
      expect(loaded.settings.policy.cpuBudgetPercent, 25);
      File('${dir.path}/settings.json').writeAsStringSync('{ broken');
      final reset = await loadSettings(dir: dir.path);
      expect(reset.resetNotice, isNotNull);
      expect(File('${dir.path}/settings.json.bad').existsSync(), isTrue);
    } finally {
      dir.deleteSync(recursive: true);
    }
  }, skip: skip);
}
