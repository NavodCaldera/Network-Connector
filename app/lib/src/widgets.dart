import 'dart:typed_data';

import 'package:flutter/material.dart';

import 'agent_api.dart';
import 'strings.dart';

/// `HH:MM` for minutes after midnight (display only).
String hhmm(int minute) =>
    '${(minute ~/ 60).toString().padLeft(2, '0')}:${(minute % 60).toString().padLeft(2, '0')}';

/// Whole number from a text field; empty or bad text gives 0, so Rust reports the problem.
BigInt parseBig(String s) {
  final v = BigInt.tryParse(s.trim());
  return (v == null || v.isNegative) ? BigInt.zero : v;
}

int parseInt(String s, {int max = 0xFFFFFFFF}) {
  final v = int.tryParse(s.trim());
  if (v == null || v < 0) return 0;
  return v > max ? max : v;
}

TimeWindow copyWindow(
  TimeWindow w, {
  List<int>? days,
  int? startMinute,
  int? endMinute,
}) => TimeWindow(
  days: days == null ? w.days : Uint8List.fromList(days),
  startMinute: startMinute ?? w.startMinute,
  endMinute: endMinute ?? w.endMinute,
);

/// Weekday chips plus From / To time pickers for one weekly window.
class WindowEditor extends StatelessWidget {
  const WindowEditor({
    super.key,
    required this.window,
    required this.onChanged,
    required this.keyPrefix,
  });

  final TimeWindow window;
  final ValueChanged<TimeWindow> onChanged;
  final String keyPrefix;

  Future<void> _pick(BuildContext context, bool start) async {
    final m = start ? window.startMinute : window.endMinute;
    final t = await showTimePicker(
      context: context,
      initialTime: TimeOfDay(hour: m ~/ 60, minute: m % 60),
    );
    if (t == null) return;
    final v = t.hour * 60 + t.minute;
    onChanged(
      start
          ? copyWindow(window, startMinute: v)
          : copyWindow(window, endMinute: v),
    );
  }

  @override
  Widget build(BuildContext context) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Wrap(
          spacing: 6,
          runSpacing: 6,
          children: [
            for (var d = 0; d < 7; d++)
              FilterChip(
                key: Key('$keyPrefix-day$d'),
                label: Text(S.days[d]),
                selected: window.days.contains(d),
                onSelected: (on) {
                  final set = window.days.toSet();
                  on ? set.add(d) : set.remove(d);
                  onChanged(copyWindow(window, days: set.toList()..sort()));
                },
              ),
          ],
        ),
        const SizedBox(height: 8),
        Row(
          children: [
            OutlinedButton(
              key: Key('$keyPrefix-start'),
              onPressed: () => _pick(context, true),
              child: Text('${S.from} ${hhmm(window.startMinute)}'),
            ),
            const SizedBox(width: 12),
            OutlinedButton(
              key: Key('$keyPrefix-end'),
              onPressed: () => _pick(context, false),
              child: Text('${S.to} ${hhmm(window.endMinute)}'),
            ),
          ],
        ),
      ],
    );
  }
}

/// Error text shown under a form, in the theme's error colour.
class ErrorText extends StatelessWidget {
  const ErrorText(this.text, {super.key});

  final String text;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.symmetric(vertical: 8),
    child: Text(
      text,
      style: TextStyle(color: Theme.of(context).colorScheme.error),
    ),
  );
}
