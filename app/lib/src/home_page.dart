import 'dart:async';

import 'package:flutter/material.dart';

import 'agent_api.dart';
import 'strings.dart';

/// The one screen of the Phase 0 shell: role, rules, live status, Stop.
class HomePage extends StatefulWidget {
  const HomePage({super.key, required this.api});

  final AgentApi api;

  @override
  State<HomePage> createState() => _HomePageState();
}

class _HomePageState extends State<HomePage> {
  late PolicySummary _policy;
  HelperState _state = HelperState.stopped;
  StreamSubscription<HelperStatus>? _sub;
  String? _error;
  Stopwatch? _stopClock;
  int? _lastStopMs;

  @override
  void initState() {
    super.initState();
    _policy = widget.api.defaultPolicy();
    _sub = widget.api.watchStatus().listen(_onStatus);
  }

  @override
  void dispose() {
    _sub?.cancel();
    super.dispose();
  }

  void _onStatus(HelperStatus s) {
    setState(() {
      _state = s.state;
      final clock = _stopClock;
      if (s.state == HelperState.stopped && clock != null) {
        _lastStopMs = clock.elapsedMilliseconds;
        _stopClock = null;
        debugPrint('stop latency: tap to Stopped = $_lastStopMs ms');
      }
    });
  }

  bool get _running =>
      _state != HelperState.stopped && _state != HelperState.stopping;

  Future<void> _give() async {
    setState(() {
      _error = null;
      _lastStopMs = null;
    });
    try {
      await widget.api.startHelperSession(_policy);
    } catch (e) {
      setState(() => _error = e.toString());
    }
  }

  Future<void> _stop() async {
    _stopClock = Stopwatch()..start();
    await widget.api.stopHelperSession();
  }

  Future<void> _setAccept(bool on) async {
    final wasRunning = _running;
    setState(() => _policy = _copyWithAccept(_policy, on));
    // Withdrawing consent must take effect now, not at the next start.
    if (wasRunning) {
      await widget.api.stopHelperSession();
      await widget.api.startHelperSession(_policy);
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Scaffold(
      appBar: AppBar(title: const Text(S.appTitle)),
      body: SafeArea(
        child: ListView(
          padding: const EdgeInsets.all(16),
          children: [
            Row(
              children: [
                Expanded(
                  child: FilledButton.icon(
                    key: const Key('give'),
                    onPressed: _running ? null : _give,
                    icon: const Icon(Icons.volunteer_activism),
                    label: const Text(S.giveRole),
                  ),
                ),
                const SizedBox(width: 12),
                const Expanded(
                  child: Tooltip(
                    message: S.askSoon,
                    child: OutlinedButton(
                      key: Key('ask'),
                      onPressed: null,
                      child: Text(S.askRole),
                    ),
                  ),
                ),
              ],
            ),
            const SizedBox(height: 16),
            Card(
              child: Padding(
                padding: const EdgeInsets.all(16),
                child: Row(
                  children: [
                    Icon(_running ? Icons.sync : Icons.pause_circle_outline),
                    const SizedBox(width: 12),
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text(
                            _stateText(_state),
                            key: const Key('status'),
                            style: theme.textTheme.titleMedium,
                          ),
                          if (_lastStopMs != null)
                            Text(S.stoppedIn(_lastStopMs!),
                                key: const Key('stopTime')),
                          if (_error != null)
                            Text(_error!,
                                style:
                                    TextStyle(color: theme.colorScheme.error)),
                        ],
                      ),
                    ),
                  ],
                ),
              ),
            ),
            const SizedBox(height: 16),
            Text(S.yourRules, style: theme.textTheme.titleMedium),
            SwitchListTile(
              key: const Key('accept'),
              title: const Text(S.acceptJobs),
              subtitle: const Text(S.acceptJobsHelp),
              value: _policy.acceptJobs,
              onChanged: _setAccept,
            ),
            _rule(Icons.memory, S.cpu(_policy.cpuBudgetPercent)),
            _rule(Icons.storage,
                S.limits(_policy.maxMemoryMb, _policy.maxDiskMb)),
            _rule(Icons.battery_charging_full,
                S.charging(_policy.chargingOnly, _policy.minBatteryPercent)),
            _rule(Icons.timer_outlined, S.maxTask(_policy.maxTaskMinutes)),
            _rule(Icons.touch_app_outlined,
                S.pauseWhenUsed(_policy.pauseOnUserActivity)),
          ],
        ),
      ),
      // Stop is always visible and always enabled (AGENT.md §1: the owner is in control).
      bottomNavigationBar: SafeArea(
        child: Padding(
          padding: const EdgeInsets.all(16),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              SizedBox(
                width: double.infinity,
                height: 56,
                child: FilledButton.icon(
                  key: const Key('stop'),
                  style: FilledButton.styleFrom(
                    backgroundColor: theme.colorScheme.error,
                    foregroundColor: theme.colorScheme.onError,
                  ),
                  onPressed: _stop,
                  icon: const Icon(Icons.stop_circle),
                  label: const Text(S.stop),
                ),
              ),
              const SizedBox(height: 4),
              Text(S.stopHelp, style: theme.textTheme.bodySmall),
            ],
          ),
        ),
      ),
    );
  }

  Widget _rule(IconData icon, String text) =>
      ListTile(dense: true, leading: Icon(icon), title: Text(text));
}

String _stateText(HelperState s) => switch (s) {
      HelperState.stopped => S.stateStopped,
      HelperState.starting => S.stateStarting,
      HelperState.waitingForJobs => S.stateWaiting,
      HelperState.notAccepting => S.stateNotAccepting,
      HelperState.stopping => S.stateStopping,
    };

PolicySummary _copyWithAccept(PolicySummary p, bool accept) => PolicySummary(
      acceptJobs: accept,
      cpuBudgetPercent: p.cpuBudgetPercent,
      maxMemoryMb: p.maxMemoryMb,
      maxDiskMb: p.maxDiskMb,
      chargingOnly: p.chargingOnly,
      minBatteryPercent: p.minBatteryPercent,
      maxTaskMinutes: p.maxTaskMinutes,
      pauseOnUserActivity: p.pauseOnUserActivity,
    );
