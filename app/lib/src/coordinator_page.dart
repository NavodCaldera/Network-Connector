import 'package:flutter/material.dart';

import 'agent_api.dart';
import 'app_model.dart';
import 'strings.dart';
import 'widgets.dart';

/// Enrol with a coordinator, or show the enrolment and offer a reset.
/// The enrolment secret is sent once by Rust and never kept by the app.
class CoordinatorPage extends StatefulWidget {
  const CoordinatorPage({super.key, required this.model});

  final AppModel model;

  @override
  State<CoordinatorPage> createState() => _CoordinatorPageState();
}

class _CoordinatorPageState extends State<CoordinatorPage> {
  final _url = TextEditingController();
  final _secret = TextEditingController();
  final _name = TextEditingController();
  final _conn = TextEditingController(text: 'home');
  final _speed = TextEditingController(text: '10');
  final _trusted = TextEditingController();
  var _network = NetworkKind.ethernet;
  var _unlimited = false;
  var _cool = false;
  var _busy = false;
  String? _error;

  @override
  void dispose() {
    for (final c in [_url, _secret, _name, _conn, _speed, _trusted]) {
      c.dispose();
    }
    super.dispose();
  }

  Future<void> _enrol() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    final err = await widget.model.enrol(
      EnrolRequest(
        coordinatorUrl: _url.text,
        enrolSecret: _secret.text,
        deviceName: _name.text,
        network: _network,
        connectionName: _conn.text,
        speedMbps: parseInt(_speed.text),
        unlimitedData: _unlimited,
        coolDevice: _cool,
        trustedPublishers: _trusted.text
            .split('\n')
            .map((l) => l.trim())
            .where((l) => l.isNotEmpty)
            .toList(),
      ),
    );
    if (!mounted) return;
    if (err == null) _secret.clear();
    setState(() {
      _busy = false;
      _error = err;
    });
  }

  Future<void> _reset() async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        content: const Text(S.resetConfirm),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text(S.cancel),
          ),
          FilledButton(
            key: const Key('resetConfirm'),
            onPressed: () => Navigator.pop(context, true),
            child: const Text(S.reset),
          ),
        ],
      ),
    );
    if (ok != true) return;
    final err = await widget.model.unenrol();
    if (mounted) setState(() => _error = err);
  }

  @override
  Widget build(BuildContext context) {
    final m = widget.model;
    final theme = Theme.of(context);
    final e = m.enrolment;
    return ListView(
      padding: const EdgeInsets.all(16),
      children: [
        Text(S.coordTitle, style: theme.textTheme.titleLarge),
        const SizedBox(height: 8),
        if (m.enrolNotice != null)
          Text(
            m.enrolNotice!,
            key: const Key('enrolNotice'),
            style: TextStyle(color: theme.colorScheme.error),
          ),
        if (m.restartNeeded)
          Text(S.restartToApply, key: const Key('restartNeeded')),
        if (e != null) ...[
          ListTile(
            key: const Key('connectedTo'),
            leading: const Icon(Icons.cloud_done_outlined),
            title: Text(S.connectedTo(e.coordinatorUrl)),
            subtitle: Text(S.deviceNamed(e.deviceName)),
          ),
          ListTile(title: Text(S.trustedCount(e.trustedPublishers))),
          if (e.deviceKey.isNotEmpty)
            ListTile(
              key: const Key('deviceKey'),
              title: const Text(S.deviceKey),
              subtitle: SelectableText(e.deviceKey),
            ),
          OutlinedButton(
            key: const Key('reset'),
            onPressed: _reset,
            child: const Text(S.reset),
          ),
        ] else ...[
          const Text(S.coordHelp),
          TextField(
            key: const Key('coordUrl'),
            controller: _url,
            keyboardType: TextInputType.url,
            autocorrect: false,
            decoration: const InputDecoration(labelText: S.coordUrl),
          ),
          TextField(
            key: const Key('enrolSecret'),
            controller: _secret,
            obscureText: true,
            autocorrect: false,
            enableSuggestions: false,
            decoration: const InputDecoration(labelText: S.enrolSecret),
          ),
          TextField(
            key: const Key('deviceName'),
            controller: _name,
            decoration: const InputDecoration(labelText: S.deviceName),
          ),
          TextField(
            key: const Key('trusted'),
            controller: _trusted,
            minLines: 2,
            maxLines: 6,
            autocorrect: false,
            decoration: const InputDecoration(
              labelText: S.trustedKeys,
              helperText: S.trustedHelp,
            ),
          ),
          const SizedBox(height: 16),
          Text(S.connectionTitle, style: theme.textTheme.titleMedium),
          const SizedBox(height: 8),
          SegmentedButton<NetworkKind>(
            segments: const [
              ButtonSegment(value: NetworkKind.ethernet, label: Text(S.wired)),
              ButtonSegment(value: NetworkKind.wifi, label: Text(S.wifi)),
            ],
            selected: {_network},
            onSelectionChanged: (v) => setState(() => _network = v.first),
          ),
          TextField(
            key: const Key('connectionName'),
            controller: _conn,
            decoration: const InputDecoration(labelText: S.connectionName),
          ),
          TextField(
            key: const Key('speed'),
            controller: _speed,
            keyboardType: TextInputType.number,
            decoration: const InputDecoration(labelText: S.speed),
          ),
          SwitchListTile(
            key: const Key('unlimited'),
            title: const Text(S.unlimited),
            subtitle: const Text(S.unlimitedHelp),
            value: _unlimited,
            onChanged: (v) => setState(() => _unlimited = v),
          ),
          SwitchListTile(
            key: const Key('cool'),
            title: const Text(S.coolDevice),
            subtitle: const Text(S.coolDeviceHelp),
            value: _cool,
            onChanged: (v) => setState(() => _cool = v),
          ),
          const SizedBox(height: 8),
          FilledButton(
            key: const Key('enrol'),
            onPressed: _busy ? null : _enrol,
            child: Text(_busy ? S.enrolling : S.enrol),
          ),
        ],
        if (_error != null)
          Padding(
            padding: const EdgeInsets.only(top: 8),
            child: Text(
              _error!,
              key: const Key('enrolError'),
              style: TextStyle(color: theme.colorScheme.error),
            ),
          ),
      ],
    );
  }
}
