import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:network_connector/main.dart';
import 'package:network_connector/src/agent_api.dart';
import 'package:network_connector/src/strings.dart';

/// In-memory agent with the same state sequence as nc-agent.
class FakeAgent implements AgentApi {
  final _ctl = StreamController<HelperStatus>.broadcast();
  var _seq = 0;
  final started = <PolicySummary>[];
  var stops = 0;

  void _emit(HelperState s) =>
      _ctl.add(HelperStatus(state: s, seq: BigInt.from(++_seq)));

  @override
  PolicySummary defaultPolicy() => PolicySummary(
        acceptJobs: false,
        cpuBudgetPercent: 25,
        maxMemoryMb: BigInt.from(256),
        maxDiskMb: BigInt.from(256),
        chargingOnly: true,
        minBatteryPercent: 20,
        maxTaskMinutes: BigInt.from(5),
        pauseOnUserActivity: true,
      );

  @override
  Future<void> startHelperSession(PolicySummary policy) async {
    started.add(policy);
    _emit(HelperState.starting);
    _emit(policy.acceptJobs
        ? HelperState.waitingForJobs
        : HelperState.notAccepting);
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

void main() {
  testWidgets('shows safe defaults and the Stop button', (tester) async {
    await tester.pumpWidget(NetworkConnectorApp(api: FakeAgent()));
    expect(find.text(S.stateStopped), findsOneWidget);
    expect(find.byKey(const Key('stop')), findsOneWidget);
    expect(find.text(S.cpu(25)), findsOneWidget);
    final sw = tester.widget<SwitchListTile>(find.byKey(const Key('accept')));
    expect(sw.value, isFalse);
    final ask = tester.widget<OutlinedButton>(find.byKey(const Key('ask')));
    expect(ask.onPressed, isNull);
  });

  testWidgets('start then Stop reaches Stopped within 1 s', (tester) async {
    final agent = FakeAgent();
    await tester.pumpWidget(NetworkConnectorApp(api: agent));

    await tester.tap(find.byKey(const Key('give')));
    await tester.pump();
    expect(find.text(S.stateNotAccepting), findsOneWidget);
    expect(agent.started.single.acceptJobs, isFalse);

    final clock = Stopwatch()..start();
    await tester.tap(find.byKey(const Key('stop')));
    await tester.pump();
    clock.stop();
    expect(find.text(S.stateStopped), findsOneWidget);
    expect(clock.elapsed, lessThan(const Duration(seconds: 1)));
    expect(find.byKey(const Key('stopTime')), findsOneWidget);
    // Stop stays visible and enabled after stopping.
    final stop = tester.widget<FilledButton>(find.byKey(const Key('stop')));
    expect(stop.onPressed, isNotNull);
  });

  testWidgets('turning Accept jobs on restarts a running session',
      (tester) async {
    final agent = FakeAgent();
    await tester.pumpWidget(NetworkConnectorApp(api: agent));
    await tester.tap(find.byKey(const Key('give')));
    await tester.pump();
    await tester.tap(find.byKey(const Key('accept')));
    await tester.pump();
    expect(agent.stops, 1);
    expect(agent.started.last.acceptJobs, isTrue);
    expect(find.text(S.stateWaiting), findsOneWidget);
  });
}
