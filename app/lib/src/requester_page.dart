import 'dart:async';

import 'package:flutter/material.dart';

import 'agent_api.dart';
import 'app_model.dart';
import 'file_picker.dart';
import 'requester_model.dart';
import 'strings.dart';

/// Ask for performance: pick a Wasm module and an input, send the job, follow it, save the result.
/// Rust reads the files and signs the job with this device's key; only paths cross the bridge.
class RequesterPage extends StatefulWidget {
  const RequesterPage({super.key, required this.model, required this.picker});

  final AppModel model;
  final FilePicker picker;

  @override
  State<RequesterPage> createState() => _RequesterPageState();
}

class _RequesterPageState extends State<RequesterPage> {
  String? _module;
  String? _input;
  var _replicas = 1;
  var _sending = false;
  String? _error;
  Timer? _timer;

  RequesterModel get _r => widget.model.requester;

  @override
  void initState() {
    super.initState();
    // Low-frequency polling of unfinished jobs while this page is open.
    _timer = Timer.periodic(
      const Duration(seconds: 15),
      (_) => _r.refreshAll(),
    );
  }

  @override
  void dispose() {
    _timer?.cancel();
    super.dispose();
  }

  static String _name(String path) =>
      path.split(RegExp(r'[\\/]')).where((s) => s.isNotEmpty).lastOrNull ??
      path;

  Future<void> _pickModule() async {
    final p = await widget.picker.openFile(
      label: S.wasmModules,
      extensions: const ['wasm'],
    );
    if (p != null) setState(() => _module = p);
  }

  Future<void> _pickInput() async {
    final p = await widget.picker.openFile(label: S.inputFile);
    if (p != null) setState(() => _input = p);
  }

  Future<void> _send() async {
    final m = _module, i = _input;
    if (m == null || i == null) return;
    setState(() {
      _sending = true;
      _error = null;
    });
    final err = await _r.submit(m, i, _replicas);
    if (!mounted) return;
    setState(() {
      _sending = false;
      _error = err;
      if (err == null) {
        _module = null;
        _input = null;
      }
    });
  }

  Future<void> _fetch(RequestedJob j) async {
    final out = await widget.picker.saveFile(
      suggestedName: 'result-${j.jobId.substring(0, 8)}.bin',
    );
    if (out == null) return;
    await _r.fetch(j.jobId, out);
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final enrolment = widget.model.enrolment;
    if (widget.model.settingsDir == null || enrolment == null) {
      return ListView(
        padding: const EdgeInsets.all(16),
        children: [
          Text(S.askTitle, style: theme.textTheme.titleLarge),
          const SizedBox(height: 8),
          const Text(S.askNeedsEnrol, key: Key('askNeedsEnrol')),
        ],
      );
    }
    return ListenableBuilder(
      listenable: _r,
      builder: (context, _) => ListView(
        padding: const EdgeInsets.all(16),
        children: [
          Text(S.askTitle, style: theme.textTheme.titleLarge),
          const SizedBox(height: 4),
          const Text(S.askHelp),
          const SizedBox(height: 12),
          Card(
            child: Padding(
              padding: const EdgeInsets.all(12),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  _fileRow('pickModule', S.module, _module, _pickModule),
                  _fileRow('pickInput', S.input, _input, _pickInput),
                  const SizedBox(height: 8),
                  const Text(S.helpers),
                  const SizedBox(height: 4),
                  SegmentedButton<int>(
                    key: const Key('replicas'),
                    segments: const [
                      ButtonSegment(value: 1, label: Text(S.oneHelper)),
                      ButtonSegment(value: 2, label: Text(S.twoHelpers)),
                    ],
                    selected: {_replicas},
                    onSelectionChanged: (s) =>
                        setState(() => _replicas = s.first),
                  ),
                  const SizedBox(height: 8),
                  Text(S.signedBy, style: theme.textTheme.bodySmall),
                  SelectableText(
                    enrolment.deviceKey,
                    key: const Key('publisherKey'),
                    style: theme.textTheme.bodySmall,
                  ),
                  const SizedBox(height: 12),
                  if (_error != null)
                    Padding(
                      padding: const EdgeInsets.only(bottom: 8),
                      child: Text(
                        _error!,
                        key: const Key('submitError'),
                        style: TextStyle(color: theme.colorScheme.error),
                      ),
                    ),
                  FilledButton.icon(
                    key: const Key('sendJob'),
                    onPressed: (_module == null || _input == null || _sending)
                        ? null
                        : _send,
                    icon: const Icon(Icons.send),
                    label: Text(_sending ? S.sending : S.send),
                  ),
                ],
              ),
            ),
          ),
          const SizedBox(height: 16),
          Row(
            children: [
              Expanded(
                child: Text(S.yourJobs, style: theme.textTheme.titleMedium),
              ),
              IconButton(
                key: const Key('refreshJobs'),
                tooltip: S.refresh,
                onPressed: _r.refreshAll,
                icon: const Icon(Icons.refresh),
              ),
            ],
          ),
          if (_r.loaded && _r.jobs.isEmpty)
            const Text(S.noJobs, key: Key('noJobs')),
          for (final j in _r.jobs) _jobCard(theme, j),
        ],
      ),
    );
  }

  Widget _fileRow(
    String key,
    String label,
    String? path,
    VoidCallback onPick,
  ) => Padding(
    padding: const EdgeInsets.only(bottom: 8),
    child: Row(
      children: [
        Expanded(
          child: Text(
            path == null ? '$label: ${S.notChosen}' : '$label: ${_name(path)}',
            key: Key('${key}Name'),
            overflow: TextOverflow.ellipsis,
          ),
        ),
        OutlinedButton(
          key: Key(key),
          onPressed: _sending ? null : onPick,
          child: const Text(S.choose),
        ),
      ],
    ),
  );

  static double _progress(JobPhase p) => switch (p) {
    JobPhase.uploading => 0.1,
    JobPhase.queued => 0.3,
    JobPhase.running => 0.6,
    JobPhase.unknown => 0,
    JobPhase.committed || JobPhase.disputed || JobPhase.failed => 1,
  };

  Widget _jobCard(ThemeData theme, RequestedJob j) {
    final busy = _r.busy.contains(j.jobId);
    final bad = j.phase == JobPhase.disputed || j.phase == JobPhase.failed;
    final problem = _r.errors[j.jobId] ?? j.message;
    final saved = _r.saved[j.jobId];
    final when = DateTime.fromMillisecondsSinceEpoch(j.createdMs.toInt());
    return Card(
      key: Key('job-${j.jobId}'),
      child: Padding(
        padding: const EdgeInsets.all(12),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(
              S.jobTitle(j.jobId.substring(0, 8), when),
              style: theme.textTheme.titleSmall,
            ),
            const SizedBox(height: 4),
            Text(S.phase(j.phase), key: Key('phase-${j.jobId}')),
            const SizedBox(height: 4),
            LinearProgressIndicator(
              value: busy && !RequesterModel.isFinal(j.phase)
                  ? null
                  : _progress(j.phase),
              color: bad ? theme.colorScheme.error : null,
            ),
            if (problem != null)
              Padding(
                padding: const EdgeInsets.only(top: 4),
                child: Text(
                  problem,
                  key: Key('jobError-${j.jobId}'),
                  style: TextStyle(color: theme.colorScheme.error),
                ),
              ),
            if (saved != null)
              Padding(
                padding: const EdgeInsets.only(top: 4),
                child: Text(saved, key: Key('saved-${j.jobId}')),
              ),
            const SizedBox(height: 8),
            Wrap(
              spacing: 8,
              children: [
                if (!RequesterModel.isFinal(j.phase))
                  OutlinedButton(
                    key: Key('check-${j.jobId}'),
                    onPressed: busy ? null : () => _r.refresh(j.jobId),
                    child: const Text(S.check),
                  ),
                if (j.phase == JobPhase.committed)
                  FilledButton.icon(
                    key: Key('fetch-${j.jobId}'),
                    onPressed: busy ? null : () => _fetch(j),
                    icon: const Icon(Icons.download),
                    label: Text(busy ? S.saving : S.saveResult),
                  ),
              ],
            ),
          ],
        ),
      ),
    );
  }
}
