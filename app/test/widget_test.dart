import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:network_connector/main.dart';
import 'package:network_connector/src/agent_api.dart';
import 'package:network_connector/src/app_model.dart';
import 'package:network_connector/src/strings.dart';

import 'fake_agent.dart';

Future<FakeAgent> _app(WidgetTester tester, [FakeAgent? agent]) async {
  final a = agent ?? FakeAgent();
  // A tall surface so long forms fit without scrolling.
  tester.view.physicalSize = const Size(1000, 2400);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(NetworkConnectorApp(api: a, settingsDir: 'test-dir'));
  await tester.pump();
  return a;
}

Future<void> _tab(WidgetTester tester, String key) async {
  await tester.tap(find.byKey(Key(key)));
  await tester.pumpAndSettle();
}

void _stopIsEnabled(WidgetTester tester) {
  final stop = tester.widget<FilledButton>(find.byKey(const Key('stop')));
  expect(stop.onPressed, isNotNull);
}

void main() {
  group('Home', () {
    testWidgets('shows safe defaults, today and the Stop button', (
      tester,
    ) async {
      await _app(tester);
      expect(find.text(S.stateStopped), findsOneWidget);
      _stopIsEnabled(tester);
      expect(find.text(S.cpu(25)), findsOneWidget);
      expect(find.text(S.noHours), findsOneWidget);
      expect(find.byKey(const Key('today')), findsOneWidget);
      expect(find.textContaining(S.helpedFor(0)), findsOneWidget);
      final sw = tester.widget<SwitchListTile>(find.byKey(const Key('accept')));
      expect(sw.value, isFalse);
      final ask = tester.widget<OutlinedButton>(find.byKey(const Key('ask')));
      expect(ask.onPressed, isNull);
    });

    testWidgets('start then Stop reaches Stopped within 1 s', (tester) async {
      final agent = await _app(tester);
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
      _stopIsEnabled(tester);
    });

    testWidgets('turning Accept jobs on restarts a running session and saves', (
      tester,
    ) async {
      final agent = await _app(tester);
      await tester.tap(find.byKey(const Key('give')));
      await tester.pump();
      await tester.tap(find.byKey(const Key('accept')));
      await tester.pump();
      expect(agent.stops, 1);
      expect(agent.started.last.acceptJobs, isTrue);
      expect(agent.saves.last.policy.acceptJobs, isTrue);
      expect(find.text(S.stateWaiting), findsOneWidget);
    });

    testWidgets('loads saved settings and shows a reset notice', (
      tester,
    ) async {
      final seed = FakeAgent().defaultSettings();
      final agent = FakeAgent(
        stored: AppSettings(
          policy: copyPolicy(seed.policy, cpuBudgetPercent: 70),
          nightData: seed.nightData,
        ),
        notice: 'Your saved settings could not be read.',
      );
      await _app(tester, agent);
      expect(find.text(S.cpu(70)), findsOneWidget);
      expect(find.byKey(const Key('resetNotice')), findsOneWidget);
    });

    testWidgets('Stop is visible and enabled on every screen', (tester) async {
      final agent = await _app(tester);
      for (final t in ['navLimits', 'navNight', 'navAbout', 'navHome']) {
        await _tab(tester, t);
        expect(find.byKey(const Key('stop')), findsOneWidget);
        _stopIsEnabled(tester);
        await tester.tap(find.byKey(const Key('stop')));
        await tester.pump();
      }
      expect(agent.stops, 4);
    });
  });

  group('Limits', () {
    testWidgets('Rust validation error is shown and nothing is saved', (
      tester,
    ) async {
      final agent = await _app(tester);
      await _tab(tester, 'navLimits');
      await tester.enterText(find.byKey(const Key('memory')), '0');
      await tester.tap(find.byKey(const Key('saveLimits')));
      await tester.pumpAndSettle();
      expect(find.byKey(const Key('limitsError')), findsOneWidget);
      expect(
        find.text('Memory limit must be greater than zero.'),
        findsOneWidget,
      );
      expect(agent.saves, isEmpty);
    });

    testWidgets('valid limits are saved and shown on Home', (tester) async {
      final agent = await _app(tester);
      await _tab(tester, 'navLimits');
      await tester.enterText(find.byKey(const Key('memory')), '512');
      await tester.tap(find.byKey(const Key('charging')));
      await tester.tap(find.byKey(const Key('hoursOn')));
      await tester.pump();
      expect(find.byKey(const Key('hours-day0')), findsOneWidget);
      await tester.tap(find.byKey(const Key('hours-day6')));
      await tester.pump();
      await tester.tap(find.byKey(const Key('saveLimits')));
      await tester.pumpAndSettle();
      expect(find.text(S.saved), findsOneWidget);
      final p = agent.saves.single.policy;
      expect(p.maxMemoryMb, BigInt.from(512));
      expect(p.chargingOnly, isFalse);
      expect(p.hours.single.days, [0, 1, 2, 3, 4, 5]);
      expect(p.hours.single.startMinute, 22 * 60);
      await _tab(tester, 'navHome');
      expect(
        find.text(S.limits(BigInt.from(512), BigInt.from(256))),
        findsOneWidget,
      );
      expect(find.text(S.hoursSet(1)), findsOneWidget);
    });

    testWidgets('saving while helping applies the new limits at once', (
      tester,
    ) async {
      final agent = await _app(tester);
      await tester.tap(find.byKey(const Key('give')));
      await tester.pump();
      await _tab(tester, 'navLimits');
      await tester.tap(find.byKey(const Key('limitsAccept')));
      await tester.pump();
      await tester.tap(find.byKey(const Key('saveLimits')));
      await tester.pumpAndSettle();
      expect(agent.stops, 1);
      expect(agent.started.last.acceptJobs, isTrue);
    });
  });

  group('Night data', () {
    testWidgets('shows the next window and budget from Rust', (tester) async {
      final agent = await _app(tester);
      await _tab(tester, 'navNight');
      expect(find.text(S.nextWindow('Tue 00:00', 'Tue 07:59')), findsOneWidget);
      expect(
        find.textContaining(S.budgetLeft('1000 MB', '1000 MB')),
        findsOneWidget,
      );
      expect(agent.statusCalls, isNotEmpty);
      final sw = tester.widget<SwitchListTile>(
        find.byKey(const Key('neverPaid')),
      );
      expect(sw.value, isTrue);
    });

    testWidgets('pick a preset, change the budget, save', (tester) async {
      final agent = await _app(tester);
      await _tab(tester, 'navNight');
      await tester.tap(find.byKey(const Key('preset-night-22-06')));
      await tester.enterText(find.byKey(const Key('budget')), '300');
      await tester.tap(find.byKey(const Key('neverPaid')));
      await tester.pump();
      await tester.tap(find.byKey(const Key('saveNight')));
      await tester.pumpAndSettle();
      final n = agent.saves.single.nightData;
      expect(n.presetId, 'night-22-06');
      expect(n.window.startMinute, 1320);
      expect(n.nightlyBudgetMb, BigInt.from(300));
      expect(n.neverPaidData, isFalse);
      expect(
        find.textContaining(S.budgetLeft('300 MB', '300 MB')),
        findsOneWidget,
      );
    });

    testWidgets('custom hours show the editor; bad budget shows the error', (
      tester,
    ) async {
      final agent = await _app(tester);
      await _tab(tester, 'navNight');
      expect(find.byKey(const Key('night-day0')), findsNothing);
      await tester.tap(find.byKey(const Key('preset-custom')));
      await tester.pump();
      expect(find.byKey(const Key('night-day0')), findsOneWidget);
      await tester.enterText(find.byKey(const Key('budget')), '0');
      await tester.tap(find.byKey(const Key('saveNight')));
      await tester.pumpAndSettle();
      expect(
        find.text('Nightly data budget must be more than 0 MB.'),
        findsOneWidget,
      );
      expect(agent.saves, isEmpty);
    });
  });

  group('About', () {
    testWidgets('shows versions and opens the licences page', (tester) async {
      await _app(tester);
      await _tab(tester, 'navAbout');
      expect(find.text(S.coreVersion('0.1.0')), findsOneWidget);
      expect(find.byKey(const Key('appVersion')), findsOneWidget);
      await tester.tap(find.byKey(const Key('licences')));
      await tester.pumpAndSettle();
      expect(find.byType(LicensePage), findsOneWidget);
    });
  });
}
