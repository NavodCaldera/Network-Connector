import 'package:flutter/material.dart';

import 'about_page.dart';
import 'app_model.dart';
import 'home_page.dart';
import 'limits_page.dart';
import 'night_data_page.dart';
import 'strings.dart';

/// Navigation plus the Stop bar. Stop sits above the navigation bar on every screen,
/// always enabled (AGENT.md §1: the owner is in control).
class AppShell extends StatefulWidget {
  const AppShell({super.key, required this.model});

  final AppModel model;

  @override
  State<AppShell> createState() => _AppShellState();
}

class _AppShellState extends State<AppShell> {
  int _tab = 0;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final m = widget.model;
    return ListenableBuilder(
      listenable: m,
      builder: (context, _) => Scaffold(
        appBar: AppBar(title: const Text(S.appTitle)),
        body: SafeArea(
          child: switch (_tab) {
            0 => HomePage(model: m),
            1 => LimitsPage(model: m),
            2 => NightDataPage(model: m),
            _ => AboutPage(model: m),
          },
        ),
        bottomNavigationBar: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 8, 16, 4),
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
                      onPressed: m.stop,
                      icon: const Icon(Icons.stop_circle),
                      label: const Text(S.stop),
                    ),
                  ),
                  const SizedBox(height: 4),
                  Text(S.stopHelp, style: theme.textTheme.bodySmall),
                ],
              ),
            ),
            NavigationBar(
              selectedIndex: _tab,
              onDestinationSelected: (i) => setState(() => _tab = i),
              destinations: const [
                NavigationDestination(
                  key: Key('navHome'),
                  icon: Icon(Icons.home),
                  label: S.navHome,
                ),
                NavigationDestination(
                  key: Key('navLimits'),
                  icon: Icon(Icons.tune),
                  label: S.navLimits,
                ),
                NavigationDestination(
                  key: Key('navNight'),
                  icon: Icon(Icons.nightlight),
                  label: S.navNight,
                ),
                NavigationDestination(
                  key: Key('navAbout'),
                  icon: Icon(Icons.info_outline),
                  label: S.navAbout,
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }
}
