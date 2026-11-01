import 'package:flutter/material.dart';

import 'agent_api.dart';
import 'app_model.dart';
import 'strings.dart';

/// Home: role, live status, today's summary and a short view of the rules.
/// Stop lives in the app shell, so it is on every screen.
class HomePage extends StatelessWidget {
  const HomePage({super.key, required this.model});

  final AppModel model;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final p = model.policy;
    final today = model.api.todaySummary();
    return ListView(
      padding: const EdgeInsets.all(16),
      children: [
        if (model.resetNotice != null)
          Card(
            color: theme.colorScheme.errorContainer,
            child: Padding(
              padding: const EdgeInsets.all(12),
              child: Text(model.resetNotice!, key: const Key('resetNotice')),
            ),
          ),
        Row(
          children: [
            Expanded(
              child: FilledButton.icon(
                key: const Key('give'),
                onPressed: model.running ? null : model.give,
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
                Icon(model.running ? Icons.sync : Icons.pause_circle_outline),
                const SizedBox(width: 12),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        stateText(model.state),
                        key: const Key('status'),
                        style: theme.textTheme.titleMedium,
                      ),
                      if (model.lastStopMs != null)
                        Text(
                          S.stoppedIn(model.lastStopMs!),
                          key: const Key('stopTime'),
                        ),
                      if (model.error != null)
                        Text(
                          model.error!,
                          key: const Key('homeError'),
                          style: TextStyle(color: theme.colorScheme.error),
                        ),
                    ],
                  ),
                ),
              ],
            ),
          ),
        ),
        Card(
          child: ListTile(
            key: const Key('today'),
            leading: const Icon(Icons.today),
            title: const Text(S.today),
            subtitle: Text(
              '${S.helpedFor((today.helpingSeconds ~/ BigInt.from(60)).toInt())}'
              ' · ${S.jobsDone(today.jobsDone.toInt())}',
            ),
          ),
        ),
        const SizedBox(height: 16),
        Text(S.yourRules, style: theme.textTheme.titleMedium),
        SwitchListTile(
          key: const Key('accept'),
          title: const Text(S.acceptJobs),
          subtitle: const Text(S.acceptJobsHelp),
          value: p.acceptJobs,
          onChanged: model.setAccept,
        ),
        _rule(Icons.memory, S.cpu(p.cpuBudgetPercent)),
        _rule(Icons.storage, S.limits(p.maxMemoryMb, p.maxDiskMb)),
        _rule(
          Icons.battery_charging_full,
          S.charging(p.chargingOnly, p.minBatteryPercent),
        ),
        _rule(Icons.timer_outlined, S.maxTask(p.maxTaskMinutes)),
        _rule(Icons.touch_app_outlined, S.pauseWhenUsed(p.pauseOnUserActivity)),
        _rule(
          Icons.schedule,
          p.hours.isEmpty ? S.noHours : S.hoursSet(p.hours.length),
        ),
      ],
    );
  }

  Widget _rule(IconData icon, String text) =>
      ListTile(dense: true, leading: Icon(icon), title: Text(text));
}

String stateText(HelperState s) => switch (s) {
  HelperState.stopped => S.stateStopped,
  HelperState.starting => S.stateStarting,
  HelperState.waitingForJobs => S.stateWaiting,
  HelperState.notAccepting => S.stateNotAccepting,
  HelperState.stopping => S.stateStopping,
};
