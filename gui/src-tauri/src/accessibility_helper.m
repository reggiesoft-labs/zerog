#import <AppKit/AppKit.h>

@interface ZeroGAccessibilityDragView : NSView <NSDraggingSource>
@property(nonatomic, copy) NSString *appBundlePath;
@property(nonatomic, strong) NSImage *dragImage;
@end

@implementation ZeroGAccessibilityDragView
- (BOOL)acceptsFirstMouse:(NSEvent *)event { return YES; }

- (void)mouseDown:(NSEvent *)event {}

- (void)mouseDragged:(NSEvent *)event {
    NSURL *appURL = [NSURL fileURLWithPath:self.appBundlePath];
    NSDraggingItem *draggingItem = [[NSDraggingItem alloc] initWithPasteboardWriter:appURL];
    NSPoint dragPoint = [self convertPoint:event.locationInWindow fromView:nil];
    NSRect dragFrame = NSMakeRect(dragPoint.x - 23, dragPoint.y - 23, 46, 46);
    [draggingItem setDraggingFrame:dragFrame contents:self.dragImage];
    [self beginDraggingSessionWithItems:@[draggingItem] event:event source:self];
}

- (NSDragOperation)draggingSession:(NSDraggingSession *)session
    sourceOperationMaskForDraggingContext:(NSDraggingContext)context {
    return NSDragOperationCopy;
}
@end

static NSPanel *accessibilityPanel;

static NSTextField *makeLabel(NSString *text, NSRect frame, CGFloat size, NSColor *color, NSFontWeight weight) {
    NSTextField *label = [[NSTextField alloc] initWithFrame:frame];
    label.stringValue = text;
    label.bezeled = NO;
    label.drawsBackground = NO;
    label.editable = NO;
    label.selectable = NO;
    label.font = [NSFont systemFontOfSize:size weight:weight];
    label.textColor = color;
    label.lineBreakMode = NSLineBreakByWordWrapping;
    label.maximumNumberOfLines = 0;
    return label;
}

static void showAccessibilityPanel(NSString *appBundlePath) {
    if (!accessibilityPanel) {
        NSRect contentRect = NSMakeRect(0, 0, 440, 340);
        NSWindowStyleMask style = NSWindowStyleMaskTitled | NSWindowStyleMaskClosable | NSWindowStyleMaskUtilityWindow;
        accessibilityPanel = [[NSPanel alloc] initWithContentRect:contentRect
                                                        styleMask:style
                                                          backing:NSBackingStoreBuffered
                                                            defer:NO];
        accessibilityPanel.title = @"Enable Accessibility";
        accessibilityPanel.level = NSFloatingWindowLevel;
        accessibilityPanel.hidesOnDeactivate = NO;
        accessibilityPanel.releasedWhenClosed = NO;
        accessibilityPanel.appearance = [NSAppearance appearanceNamed:NSAppearanceNameDarkAqua];
        NSView *contentView = accessibilityPanel.contentView;
        contentView.wantsLayer = YES;
        contentView.layer.backgroundColor = [NSColor colorWithCalibratedRed:0.08 green:0.10 blue:0.14 alpha:1].CGColor;

        [contentView addSubview:makeLabel(@"MACOS ACCESSIBILITY",
                                          NSMakeRect(28, 297, 384, 16),
                                          11,
                                          [NSColor colorWithCalibratedRed:0.53 green:0.66 blue:0.82 alpha:1],
                                          NSFontWeightBold)];
        [contentView addSubview:makeLabel(@"Let ZeroG control your mouse",
                                          NSMakeRect(28, 257, 384, 30),
                                          22,
                                          [NSColor whiteColor],
                                          NSFontWeightSemibold)];
        [contentView addSubview:makeLabel(@"Drag this row into Privacy & Security > Accessibility.",
                                          NSMakeRect(28, 220, 384, 24),
                                          14,
                                          [NSColor colorWithCalibratedWhite:0.78 alpha:1],
                                          NSFontWeightRegular)];

        NSView *appCard = [[NSView alloc] initWithFrame:NSMakeRect(28, 128, 384, 72)];
        appCard.wantsLayer = YES;
        appCard.layer.backgroundColor = [NSColor colorWithCalibratedRed:0.13 green:0.17 blue:0.23 alpha:1].CGColor;
        appCard.layer.borderColor = [NSColor colorWithCalibratedRed:0.27 green:0.34 blue:0.44 alpha:1].CGColor;
        appCard.layer.borderWidth = 1;
        appCard.layer.cornerRadius = 14;

        NSImage *appIcon = nil;
        if (appBundlePath) {
            appIcon = [[NSWorkspace sharedWorkspace] iconForFile:appBundlePath];
            NSImageView *iconView = [[NSImageView alloc] initWithFrame:NSMakeRect(14, 12, 48, 48)];
            iconView.image = appIcon;
            iconView.imageScaling = NSImageScaleProportionallyUpOrDown;
            [appCard addSubview:iconView];
        }

        [appCard addSubview:makeLabel(@"ZeroG",
                                      NSMakeRect(76, 26, 130, 20),
                                      16,
                                      [NSColor whiteColor],
                                      NSFontWeightSemibold)];
        if (appBundlePath) {
            NSView *dragBadge = [[NSView alloc] initWithFrame:NSMakeRect(270, 22, 98, 28)];
            dragBadge.wantsLayer = YES;
            dragBadge.layer.backgroundColor = [[NSColor controlAccentColor] colorWithAlphaComponent:0.14].CGColor;
            dragBadge.layer.cornerRadius = 14;
            [appCard addSubview:dragBadge];

            NSTextField *dragLabel = makeLabel(@"Drag anywhere",
                                               NSMakeRect(0, 6, 98, 16),
                                               12,
                                               [NSColor colorWithCalibratedRed:0.42 green:0.70 blue:1 alpha:1],
                                               NSFontWeightRegular);
            dragLabel.alignment = NSTextAlignmentCenter;
            [dragBadge addSubview:dragLabel];

            ZeroGAccessibilityDragView *dragView = [[ZeroGAccessibilityDragView alloc]
                initWithFrame:appCard.bounds];
            dragView.appBundlePath = appBundlePath;
            dragView.dragImage = appIcon;
            [appCard addSubview:dragView];
        }

        [contentView addSubview:appCard];

        NSBox *separator = [[NSBox alloc] initWithFrame:NSMakeRect(28, 102, 384, 1)];
        separator.boxType = NSBoxSeparator;
        [contentView addSubview:separator];

        NSString *fallback = appBundlePath
            ? @"Can't drag? Click + and choose ZeroG from Applications."
            : @"Click + in Accessibility and choose the ZeroG app.";
        [contentView addSubview:makeLabel(fallback,
                                          NSMakeRect(28, 48, 384, 38),
                                          13,
                                          [NSColor colorWithCalibratedWhite:0.70 alpha:1],
                                          NSFontWeightRegular)];
    }

    NSScreen *screen = [NSScreen mainScreen];
    NSRect screenFrame = screen.visibleFrame;
    NSRect windowFrame = accessibilityPanel.frame;
    windowFrame.origin.x = NSMinX(screenFrame) + 24;
    windowFrame.origin.y = NSMaxY(screenFrame) - NSHeight(windowFrame) - 48;
    [accessibilityPanel setFrameOrigin:windowFrame.origin];
    [accessibilityPanel makeKeyAndOrderFront:nil];
    [[NSApplication sharedApplication] activateIgnoringOtherApps:YES];
}

void zerog_show_accessibility_helper(const char *app_bundle_path) {
    NSString *appBundlePath = app_bundle_path
        ? [NSString stringWithUTF8String:app_bundle_path]
        : nil;
    dispatch_async(dispatch_get_main_queue(), ^{
        @autoreleasepool {
            showAccessibilityPanel(appBundlePath);
        }
    });
}
