import 'dart:typed_data';

import 'package:flutter/material.dart';

import 'agent_api.dart';
import 'app_model.dart';
import 'strings.dart';
import 'widgets.dart';

/// Owner limits (nc-policy OwnerPolicy). Rust validates on Save; errors are shown as they come.
class LimitsPage extends StatefulWidget {
  const LimitsPage({super.key, required this.model});

  final AppModel model;

  @override
  State<LimitsPage> createState() => _LimitsPageState();
}

class _LimitsPageState extends State<LimitsPage> {
  late PolicySummary _p;
  late final TextEditingController _mem, _disk, _task, _idle, _tz;
  String? _error;

  @override
  void initState() {
    super.initState();
    _p = widget.model.policy;
    _mem = TextEditingController(text: '${_p.maxMemoryMb}');
    _disk = TextEditingController(text: '${_p.maxDiskMb}');
    _task = TextEditingController(text: '${_p.maxTaskMinutes}');
    _idle = TextEditingController(text: '${_p.idleMinutes}');
    _tz = TextEditingController(text: _p.timezone);
  }

  @override
  void dispose() {
    for (final c in [_mem, _disk, _task, _idle, _tz]) {
      c.dispose();
    }
    super.dispose();
  }

  PolicySummary _draft() => copyPolicy(
    _p,
    maxMemoryMb: parseBig(_mem.text),
    maxDiskMb: parseBig(_disk.text),
    maxTaskMinutes: parseBig(_task.text),
    idleMinutes: parseInt(_idle.text),
    timezone: _tz.text.trim(),
  );

  Future<void> _save() async {
    final err = await widget.model.savePolicy(_draft());
    if (!mounted) return;
    setState(() => _error = err);
    if (err == null) {
      ScaffoldMessenger.of(context)
          .showSnackBar(const SnackBar(content: Text(S.saved)));
    }
  }

  Widget _number(String key, String label, TextEditingController c) => Padding(
    padding: const EdgeInsets.symmetric(vertical: 6),
    child: TextField(
      key: Key(key),
      controller: c,
      keyboardType: TextInputType.number,
      decoration: InputDecoration(
        labelText: label,
        border: const OutlineInputBorder(),
      ),
    ),
  );

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final hasHours = _p.hours.isNotEmpty;
    return ListView(
      padding: const EdgeInsets.all(16),
      children: [
        Text(S.limitsTitle, style: theme.textTheme.titleLarge),
        SwitchListTile(
          key: const Key('limitsAccept'),
          title: const Text(S.acceptJobs),
          subtitle: const Text(S.acceptJobsHelp),
          value: _p.acceptJobs,
          onChanged: (v) => setState(() => _p = copyPolicy(_p, acceptJobs: v)),
        ),
        ListTile(
          title: Text(S.cpuLabel(_p.cpuBudgetPercent)),
          subtitle: const Text(S.cpuHelp),
        ),
        Slider(
          key: const Key('cpu'),
          min: 1,
          max: 100,
          divisions: 99,
          value: _p.cpuBudgetPercent.clamp(1, 100).toDouble(),
          label: '${_p.cpuBudgetPercent}%',
          onChanged: (v) =>
              setState(() => _p = copyPolicy(_p, cpuBudgetPercent: v.round())),
        ),
        _number('memory', S.memoryLabel, _mem),
        _number('disk', S.diskLabel, _disk),
        _number('maxTask', S.maxTaskLabel, _task),
        SwitchListTile(
          key: const Key('charging'),
          title: const Text(S.chargingOnly),
          value: _p.chargingOnly,
          onChanged: (v) =>
              setState(() => _p = copyPolicy(_p, chargingOnly: v)),
        ),
        ListTile(title: Text(S.batteryLabel(_p.minBatteryPercent))),
        Slider(
          key: const Key('battery'),
          min: 0,
          max: 100,
          divisions: 20,
          value: _p.minBatteryPercent.clamp(0, 100).toDouble(),
          onChanged: (v) =>
              setState(() => _p = copyPolicy(_p, minBatteryPercent: v.round())),
        ),
        SwitchListTile(
          key: const Key('idle'),
          title: const Text(S.idleOnly),
          value: _p.pauseOnUserActivity,
          onChanged: (v) =>
              setState(() => _p = copyPolicy(_p, pauseOnUserActivity: v)),
        ),
        if (_p.pauseOnUserActivity)
          _number('idleMinutes', S.idleMinutesLabel, _idle),
        const SizedBox(height: 8),
        Text(S.hoursTitle, style: theme.textTheme.titleMedium),
        SwitchListTile(
          key: const Key('hoursOn'),
          title: const Text(S.hoursOn),
          subtitle: hasHours ? null : const Text(S.hoursOff),
          value: hasHours,
          onChanged: (v) => setState(
            () => _p = copyPolicy(
              _p,
              hours: v
                  ? [
                      TimeWindow(
                        days: Uint8List.fromList([0, 1, 2, 3, 4, 5, 6]),
                        startMinute: 22 * 60,
                        endMinute: 6 * 60,
                      ),
                    ]
                  : const [],
            ),
          ),
        ),
        if (hasHours)
          WindowEditor(
            keyPrefix: 'hours',
            window: _p.hours.first,
            onChanged: (w) => setState(() => _p = copyPolicy(_p, hours: [w])),
          ),
        Padding(
          padding: const EdgeInsets.symmetric(vertical: 6),
          child: TextField(
            key: const Key('timezone'),
            controller: _tz,
            decoration: const InputDecoration(
              labelText: S.timezoneLabel,
              border: OutlineInputBorder(),
            ),
          ),
        ),
        if (_error != null) ErrorText(_error!, key: const Key('limitsError')),
        const SizedBox(height: 8),
        FilledButton(
          key: const Key('saveLimits'),
          onPressed: _save,
          child: const Text(S.save),
        ),
      ],
    );
  }
}
