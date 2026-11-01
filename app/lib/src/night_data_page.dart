import 'package:flutter/material.dart';

import 'agent_api.dart';
import 'app_model.dart';
import 'strings.dart';
import 'widgets.dart';

const _custom = 'custom';

/// Night-data (tariff) settings. The next free window and the budget left come from nc-tariff in Rust.
class NightDataPage extends StatefulWidget {
  const NightDataPage({super.key, required this.model});

  final AppModel model;

  @override
  State<NightDataPage> createState() => _NightDataPageState();
}

class _NightDataPageState extends State<NightDataPage> {
  late NightDataSettings _n;
  late final List<TariffPreset> _presets;
  late final TextEditingController _budget;
  String? _error;

  @override
  void initState() {
    super.initState();
    _n = widget.model.settings.nightData;
    _presets = widget.model.api.tariffPresets();
    _budget = TextEditingController(text: '${_n.nightlyBudgetMb}');
  }

  @override
  void dispose() {
    _budget.dispose();
    super.dispose();
  }

  NightDataSettings _copy({
    String? presetId,
    TimeWindow? window,
    bool? neverPaidData,
  }) => NightDataSettings(
    presetId: presetId ?? _n.presetId,
    window: window ?? _n.window,
    nightlyBudgetMb: parseBig(_budget.text),
    neverPaidData: neverPaidData ?? _n.neverPaidData,
    timezone: _n.timezone,
  );

  Future<void> _save() async {
    final err = await widget.model.saveNightData(_copy());
    if (!mounted) return;
    setState(() => _error = err);
    if (err == null) {
      ScaffoldMessenger.of(context)
          .showSnackBar(const SnackBar(content: Text(S.saved)));
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return ListView(
      padding: const EdgeInsets.all(16),
      children: [
        Text(S.nightTitle, style: theme.textTheme.titleLarge),
        const SizedBox(height: 4),
        const Text(S.nightHelp),
        const SizedBox(height: 12),
        _StatusCard(model: widget.model),
        const SizedBox(height: 12),
        Wrap(
          spacing: 8,
          runSpacing: 8,
          children: [
            for (final p in _presets)
              ChoiceChip(
                key: Key('preset-${p.id}'),
                label: Text(S.presetLabel(p.id)),
                selected: _n.presetId == p.id,
                onSelected: (_) => setState(
                  () => _n = _copy(presetId: p.id, window: p.window),
                ),
              ),
            ChoiceChip(
              key: const Key('preset-custom'),
              label: Text(S.presetLabel(_custom)),
              selected: _n.presetId == _custom,
              onSelected: (_) => setState(() => _n = _copy(presetId: _custom)),
            ),
          ],
        ),
        const SizedBox(height: 12),
        if (_n.presetId == _custom)
          WindowEditor(
            keyPrefix: 'night',
            window: _n.window,
            onChanged: (w) => setState(() => _n = _copy(window: w)),
          ),
        Padding(
          padding: const EdgeInsets.symmetric(vertical: 8),
          child: TextField(
            key: const Key('budget'),
            controller: _budget,
            keyboardType: TextInputType.number,
            decoration: const InputDecoration(
              labelText: S.budgetLabel,
              border: OutlineInputBorder(),
            ),
          ),
        ),
        SwitchListTile(
          key: const Key('neverPaid'),
          title: const Text(S.neverPaid),
          subtitle: Text(
            _n.neverPaidData ? S.neverPaidHelp : S.paidAllowedHelp,
          ),
          value: _n.neverPaidData,
          onChanged: (v) => setState(() => _n = _copy(neverPaidData: v)),
        ),
        if (_error != null) ErrorText(_error!, key: const Key('nightError')),
        FilledButton(
          key: const Key('saveNight'),
          onPressed: _save,
          child: const Text(S.save),
        ),
      ],
    );
  }
}

/// Shows the saved settings' next window and budget, as computed by Rust.
class _StatusCard extends StatelessWidget {
  const _StatusCard({required this.model});

  final AppModel model;

  @override
  Widget build(BuildContext context) {
    NightDataStatus? s;
    String? err;
    try {
      s = model.api.nightDataStatus(model.settings.nightData);
    } catch (e) {
      err = e.toString();
    }
    final String line;
    if (s == null) {
      line = err ?? S.noWindow;
    } else if (s.openNow) {
      line = '${S.openNow}. ${S.openUntil(s.windowEnd ?? '')}';
    } else if (s.windowStart != null) {
      line = S.nextWindow(s.windowStart!, s.windowEnd ?? '');
    } else {
      line = S.noWindow;
    }
    return Card(
      child: ListTile(
        leading: Icon(s?.openNow == true ? Icons.wifi : Icons.nightlight),
        title: Text(line, key: const Key('nightWindow')),
        subtitle: s == null
            ? null
            : Text(
                '${S.budgetLeft(S.mb(s.budgetLeftBytes), S.mb(s.budgetTotalBytes))}\n${S.budgetEstimate}',
                key: const Key('nightBudget'),
              ),
      ),
    );
  }
}
