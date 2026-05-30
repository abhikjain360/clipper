import 'package:clipper_app/main.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  testWidgets('shows startup errors instead of a blank screen', (
    WidgetTester tester,
  ) async {
    await tester.pumpWidget(const ClipperApp(startupError: 'runtime failed'));

    expect(find.text('Cannot start Clipper'), findsOneWidget);
    expect(find.text('runtime failed'), findsOneWidget);
  });
}
