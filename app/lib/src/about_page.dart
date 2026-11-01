import 'package:flutter/material.dart';

import 'app_model.dart';
import 'strings.dart';

/// Keep in step with `version:` in pubspec.yaml.
const appVersion = '1.0.0';

class AboutPage extends StatelessWidget {
  const AboutPage({super.key, required this.model});

  final AppModel model;

  @override
  Widget build(BuildContext context) {
    final core = model.api.coreVersion();
    return ListView(
      padding: const EdgeInsets.all(16),
      children: [
        Text(S.aboutTitle, style: Theme.of(context).textTheme.titleLarge),
        const SizedBox(height: 8),
        const Text(S.aboutText),
        ListTile(
          key: const Key('appVersion'),
          title: Text(S.appVersion(appVersion)),
        ),
        ListTile(
          key: const Key('coreVersion'),
          title: Text(S.coreVersion(core)),
        ),
        ListTile(
          key: const Key('licences'),
          leading: const Icon(Icons.description_outlined),
          title: const Text(S.licences),
          onTap: () => showLicensePage(
            context: context,
            applicationName: S.appTitle,
            applicationVersion: '$appVersion (core $core)',
          ),
        ),
      ],
    );
  }
}
