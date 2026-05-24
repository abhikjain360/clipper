import 'package:clipper_app/main.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  testWidgets('App smoke test placeholder', (WidgetTester tester) async {
    // TODO: add real widget tests
    expect(1 + 1, equals(2));
  });

  testWidgets('shows startup errors instead of a blank screen', (
    WidgetTester tester,
  ) async {
    await tester.pumpWidget(const ClipperApp(startupError: 'runtime failed'));

    expect(find.text('Cannot start Clipper'), findsOneWidget);
    expect(find.text('runtime failed'), findsOneWidget);
  });
}
