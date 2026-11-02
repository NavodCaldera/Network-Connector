import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:network_connector/main.dart';
import 'package:network_connector/src/agent_api.dart';
import 'package:network_connector/src/file_picker.dart';
import 'package:network_connector/src/strings.dart';

import 'fake_agent.dart';

class FakePicker implements FilePicker {
  final opens = <String?>[];
  String? save = r'C:\out\result.bin';

  @override
  Future<String?> openFile({
    required String label,
    List<String>? extensions,
  }) async => opens.isEmpty ? null : opens.removeAt(0);

  @override
  Future<String?> saveFile({required String suggestedName}) async => save;
}

FakeAgent _enrolled() => FakeAgent()
  ..enrolled = const Enrolment(
    coordinatorUrl: 'https://c.example',
    deviceName: 'pc',
    unlimitedData: true,
    trustedPublishers: 1,
    deviceKey: 'DEVICEPUBKEY',
    active: true,
  );

Future<void> _open(
  WidgetTester tester,
  FakeAgent a,
  FakePicker p, {
  bool viaHome = false,
}) async {
  tester.view.physicalSize = const Size(1000, 2400);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    NetworkConnectorApp(api: a, settingsDir: 'test-dir', picker: p),
  );
  await tester.pumpAndSettle();
  await tester.tap(find.byKey(Key(viaHome ? 'ask' : 'navAsk')));
  await tester.pumpAndSettle();
}

FilledButton _stop(WidgetTester tester) =>
    tester.widget<FilledButton>(find.byKey(const Key('stop')));

void main() {
  testWidgets('not enrolled: explains, and Stop is still there', (
    tester,
  ) async {
    await _open(tester, FakeAgent(), FakePicker());
    expect(find.byKey(const Key('askNeedsEnrol')), findsOneWidget);
    expect(_stop(tester).onPressed, isNotNull);
  });

  testWidgets('pick files, choose 2 helpers, send; job shows queued', (
    tester,
  ) async {
    final a = _enrolled();
    final p = FakePicker()
      ..opens.addAll([r'C:\jobs\sum.wasm', r'C:\jobs\numbers.txt']);
    await _open(tester, a, p, viaHome: true);
    expect(find.text(S.askTitle), findsOneWidget);
    expect(find.text('DEVICEPUBKEY'), findsOneWidget);
    expect(find.byKey(const Key('noJobs')), findsOneWidget);
    final send = find.byKey(const Key('sendJob'));
    expect(tester.widget<FilledButton>(send).onPressed, isNull);

    await tester.tap(find.byKey(const Key('pickModule')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('pickInput')));
    await tester.pumpAndSettle();
    expect(find.text('${S.module}: sum.wasm'), findsOneWidget);
    expect(find.text('${S.input}: numbers.txt'), findsOneWidget);
    await tester.tap(find.text(S.twoHelpers));
    await tester.pumpAndSettle();
    await tester.tap(send);
    await tester.pumpAndSettle();

    expect(a.submits.single.modulePath, r'C:\jobs\sum.wasm');
    expect(a.submits.single.inputPath, r'C:\jobs\numbers.txt');
    expect(a.submits.single.replicas, 2);
    final id = a.jobs.single.jobId;
    expect(find.text(S.phase(JobPhase.queued)), findsOneWidget);
    expect(find.byKey(Key('check-$id')), findsOneWidget);
    expect(find.byKey(const Key('noJobs')), findsNothing);
  });

  testWidgets('a rejected module shows the Rust message', (tester) async {
    final a = _enrolled();
    final p = FakePicker()..opens.addAll([r'C:\a.txt', r'C:\b.txt']);
    await _open(tester, a, p);
    await tester.tap(find.byKey(const Key('pickModule')));
    await tester.tap(find.byKey(const Key('pickInput')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('sendJob')));
    await tester.pumpAndSettle();
    expect(
      find.text('The module file is not a WebAssembly (.wasm) module.'),
      findsOneWidget,
    );
    expect(a.jobs, isEmpty);
  });

  testWidgets('jobs survive a restart and are checked; result is saved', (
    tester,
  ) async {
    final a = _enrolled()
      ..jobs = [
        RequestedJob(
          jobId: 'aaaaaaaa-0000-4000-8000-000000000000',
          phase: JobPhase.uploading,
          createdMs: 1700000000000,
        ),
      ]
      ..nextPhase = JobPhase.committed;
    final p = FakePicker();
    await _open(tester, a, p);
    const id = 'aaaaaaaa-0000-4000-8000-000000000000';
    // Loaded from the records and checked once at start.
    expect(a.jobStatusCalls, 1);
    expect(find.text(S.phase(JobPhase.committed)), findsOneWidget);
    await tester.tap(find.byKey(const Key('fetch-$id')));
    await tester.pumpAndSettle();
    expect(a.fetches.single, r'C:\out\result.bin');
    expect(find.byKey(const Key('saved-$id')), findsOneWidget);
    expect(find.textContaining('Checksum verified'), findsOneWidget);
  });

  testWidgets('offline and fetch errors are shown on the job', (tester) async {
    const id = 'bbbbbbbb-0000-4000-8000-000000000000';
    final a = _enrolled()
      ..jobs = [
        RequestedJob(
          jobId: id,
          phase: JobPhase.committed,
          createdMs: 1700000000000,
        ),
      ]
      ..jobError =
          'The downloaded result did not match its checksum. It was not saved.';
    await _open(tester, a, FakePicker());
    // Final jobs are not re-checked at start.
    expect(a.jobStatusCalls, 0);
    await tester.tap(find.byKey(const Key('fetch-$id')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('jobError-$id')), findsOneWidget);
    expect(find.byKey(const Key('saved-$id')), findsNothing);
  });

  testWidgets('cancelled save dialog does nothing', (tester) async {
    const id = 'cccccccc-0000-4000-8000-000000000000';
    final a = _enrolled()
      ..jobs = [
        RequestedJob(
          jobId: id,
          phase: JobPhase.committed,
          createdMs: 1700000000000,
        ),
      ];
    final p = FakePicker()..save = null;
    await _open(tester, a, p);
    await tester.tap(find.byKey(const Key('fetch-$id')));
    await tester.pumpAndSettle();
    expect(a.fetches, isEmpty);
  });

  testWidgets('a failed check shows the reason and keeps the job', (
    tester,
  ) async {
    const id = 'dddddddd-0000-4000-8000-000000000000';
    final a = _enrolled()
      ..jobs = [RequestedJob(jobId: id, phase: JobPhase.queued, createdMs: 1)]
      ..jobError = 'Cannot reach the coordinator.';
    await _open(tester, a, FakePicker());
    expect(find.byKey(const Key('jobError-$id')), findsOneWidget);
    expect(find.text(S.phase(JobPhase.queued)), findsOneWidget);
    expect(_stop(tester).onPressed, isNotNull);
  });
}
