#import <AppKit/AppKit.h>
#include <stdbool.h>

extern char *guest_chat_request(void *state, const char *request);
extern void guest_chat_free(char *response);

static NSString *String(id value) {
    return [value isKindOfClass:NSString.class] ? value : @"";
}

static NSImage *Image(id value) {
    NSData *data = [[NSData alloc] initWithBase64EncodedString:String(value) options:0];
    return data.length ? [[NSImage alloc] initWithData:data] : nil;
}

static NSButton *Button(NSString *title, id target, SEL action) {
    return [NSButton buttonWithTitle:title target:target action:action];
}

static NSStackView *Stack(NSArray<NSView *> *views, NSUserInterfaceLayoutOrientation orientation) {
    NSStackView *stack = [NSStackView stackViewWithViews:views];
    stack.orientation = orientation;
    stack.spacing = 8;
    stack.alignment = orientation == NSUserInterfaceLayoutOrientationVertical
        ? NSLayoutAttributeLeading : NSLayoutAttributeCenterY;
    return stack;
}

static NSView *Spacer(void) {
    NSView *view = [NSView new];
    [view setContentHuggingPriority:NSLayoutPriorityDefaultLow forOrientation:NSLayoutConstraintOrientationHorizontal];
    return view;
}

@interface GuestChatRoot : NSView
@property (copy) BOOL (^dropFiles)(NSArray<NSURL *> *);
@end

@implementation GuestChatRoot
- (NSDragOperation)draggingEntered:(id<NSDraggingInfo>)info {
    return [info.draggingPasteboard canReadObjectForClasses:@[NSURL.class]
        options:@{NSPasteboardURLReadingFileURLsOnlyKey: @YES}] ? NSDragOperationCopy : NSDragOperationNone;
}
- (BOOL)performDragOperation:(id<NSDraggingInfo>)info {
    NSArray *urls = [info.draggingPasteboard readObjectsForClasses:@[NSURL.class]
        options:@{NSPasteboardURLReadingFileURLsOnlyKey: @YES}];
    return urls.count && self.dropFiles(urls);
}
@end

static NSAttributedStringKey const MessageBubble = @"BoxenMessageBubble";
static NSAttributedStringKey const OriginalParagraph = @"BoxenOriginalParagraph";

@interface GuestBubble : NSObject
@property BOOL sent;
@property CGFloat preferredWidth;
@end
@implementation GuestBubble
@end

static NSRect BubbleRect(GuestBubble *bubble, CGFloat containerWidth) {
    CGFloat width = MIN(MAX(120, bubble.preferredWidth + 28), floor(containerWidth * 0.74));
    return NSMakeRect(bubble.sent ? containerWidth - width - 2 : 2, 0, width, 0);
}

static void LayoutBubbles(NSMutableAttributedString *text, NSRange range, CGFloat width) {
    [text enumerateAttribute:MessageBubble inRange:range options:0
        usingBlock:^(GuestBubble *bubble, NSRange message, BOOL * __unused stop) {
        if (!bubble) return;
        NSRect rect = BubbleRect(bubble, width);
        CGFloat leading = NSMinX(rect) + 8;
        CGFloat trailing = NSMaxX(rect) - width - 8;
        [text enumerateAttribute:OriginalParagraph inRange:message options:0
            usingBlock:^(NSParagraphStyle *original, NSRange paragraph, BOOL * __unused stopParagraph) {
            NSTextTableBlock *cell = (NSTextTableBlock *)original.textBlocks.firstObject;
            if ([cell isKindOfClass:NSTextTableBlock.class]) {
                // A table lays out its cells independently of paragraph indents.
                NSTextTable *table = cell.table;
                [table setWidth:leading type:NSTextBlockAbsoluteValueType
                    forLayer:NSTextBlockMargin edge:NSMinXEdge];
                [table setValue:NSWidth(rect) - 16 type:NSTextBlockAbsoluteValueType
                    forDimension:NSTextBlockMaximumWidth];
                [text addAttribute:NSParagraphStyleAttributeName value:[original mutableCopy] range:paragraph];
                return;
            }
            NSMutableParagraphStyle *style = [original mutableCopy];
            style.firstLineHeadIndent += leading;
            style.headIndent += leading;
            style.tailIndent = style.tailIndent <= 0 ? style.tailIndent + trailing : style.tailIndent + leading;
            [text addAttribute:NSParagraphStyleAttributeName value:style range:paragraph];
        }];
    }];
}

// TextKit owns layout, text input and selection. This draws only the message
// background, before TextKit draws any selection highlight.
@interface GuestHistoryLayout : NSLayoutManager
@end

@implementation GuestHistoryLayout
- (void)drawBackgroundForGlyphRange:(NSRange)glyphs atPoint:(NSPoint)origin {
    NSTextStorage *storage = self.textStorage;
    NSRange visible = [self characterRangeForGlyphRange:glyphs actualGlyphRange:NULL];
    NSTextContainer *container = self.textContainers.firstObject;
    BOOL dark = [[NSAppearance.currentDrawingAppearance
        bestMatchFromAppearancesWithNames:@[NSAppearanceNameAqua, NSAppearanceNameDarkAqua]]
        isEqualToString:NSAppearanceNameDarkAqua];
    for (NSUInteger cursor = visible.location; cursor < NSMaxRange(visible);) {
        NSRange message;
        GuestBubble *bubble = [storage attribute:MessageBubble atIndex:cursor
            longestEffectiveRange:&message inRange:NSMakeRange(0, storage.length)];
        cursor = NSMaxRange(message);
        if (!bubble) continue;
        NSRange messageGlyphs = [self glyphRangeForCharacterRange:message actualCharacterRange:NULL];
        NSRect text = [self boundingRectForGlyphRange:messageGlyphs inTextContainer:container];
        NSRect rect = BubbleRect(bubble, container.containerSize.width);
        rect.origin.x += origin.x;
        rect.origin.y = NSMinY(text) + origin.y - 3;
        rect.size.height = NSHeight(text) + 7;
        NSColor *color = bubble.sent
            ? (dark ? [NSColor colorWithSRGBRed:0.12 green:0.25 blue:0.21 alpha:1]
                    : [NSColor colorWithSRGBRed:0.87 green:0.94 blue:0.89 alpha:1])
            : (dark ? [NSColor colorWithSRGBRed:0.17 green:0.19 blue:0.20 alpha:1]
                    : [NSColor colorWithSRGBRed:0.93 green:0.95 blue:0.96 alpha:1]);
        [color setFill];
        [[NSBezierPath bezierPathWithRoundedRect:rect xRadius:10 yRadius:10] fill];
    }
    [super drawBackgroundForGlyphRange:glyphs atPoint:origin];
}
@end

@interface GuestComposer : NSTextView
@property (copy) void (^sendMessage)(void);
@end

@implementation GuestComposer
- (void)insertNewline:(id)sender {
    NSEventModifierFlags flags = NSApp.currentEvent.modifierFlags;
    if ((flags & NSEventModifierFlagShift) || self.hasMarkedText) {
        [super insertNewline:sender];
    } else if (self.sendMessage) {
        self.sendMessage();
    }
}
@end

@interface GuestChat : NSObject <NSApplicationDelegate, NSWindowDelegate, NSTextViewDelegate>
@property void *state;
@property BOOL setup;
@property (nonatomic) BOOL checking;
@property (nonatomic) BOOL renderingHistory;
@property NSUInteger messageCount;
@property CGFloat bubbleLayoutWidth;
@property (strong) NSWindow *window;
@property (strong) NSTimer *timer;
@property (strong) NSTextView *history;
@property (strong) GuestComposer *composer;
@property (strong) NSTextView *details;
@property (strong) NSTextField *title;
@property (strong) NSTextField *presence;
@property (strong) NSTextField *connection;
@property (strong) NSTextField *notice;
@property (strong) NSStackView *drafts;
@property (strong) NSScrollView *draftScroll;
@property (strong) NSLayoutConstraint *draftHeight;
@property (strong) NSButton *chooseKey;
@property (strong) NSButton *connectButton;
@property (strong) NSURL *key;
@end

@implementation GuestChat
- (id)request:(NSDictionary *)request {
    NSData *json = [NSJSONSerialization dataWithJSONObject:request options:0 error:nil];
    NSString *input = [[NSString alloc] initWithData:json encoding:NSUTF8StringEncoding];
    char *response = guest_chat_request(self.state, input.UTF8String);
    NSData *data = [NSData dataWithBytes:response length:strlen(response)];
    guest_chat_free(response);
    NSDictionary *result = [NSJSONSerialization JSONObjectWithData:data options:0 error:nil];
    if (result[@"error"]) {
        self.notice.stringValue = String(result[@"error"]);
        return nil;
    }
    id value = result[@"value"];
    return value == NSNull.null ? nil : value;
}

- (void)applicationDidFinishLaunching:(NSNotification *)notification {
    self.window = [[NSWindow alloc] initWithContentRect:NSMakeRect(0, 0, 790, 650)
        styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable |
                  NSWindowStyleMaskMiniaturizable | NSWindowStyleMaskResizable
        backing:NSBackingStoreBuffered defer:NO];
    self.window.title = @"Boxen Guest Chat";
    self.window.minSize = NSMakeSize(450, 380);
    self.window.delegate = self;
    [self buildWindow];
    [self.window center];
    [self.window makeKeyAndOrderFront:nil];
    [NSApp activateIgnoringOtherApps:YES];
    [self refresh];
    self.timer = [NSTimer timerWithTimeInterval:0.25 target:self selector:@selector(refresh)
        userInfo:nil repeats:YES];
    [NSRunLoop.mainRunLoop addTimer:self.timer forMode:NSDefaultRunLoopMode];
}

- (void)windowWillClose:(NSNotification *)notification {
    [NSApp terminate:nil];
}

- (void)applicationWillTerminate:(NSNotification *)notification {
    [self.timer invalidate];
}

- (void)buildWindow {
    GuestChatRoot *root = [GuestChatRoot new];
    root.translatesAutoresizingMaskIntoConstraints = NO;
    [root registerForDraggedTypes:@[NSPasteboardTypeFileURL]];
    __weak GuestChat *weakSelf = self;
    root.dropFiles = ^BOOL(NSArray<NSURL *> *files) { return [weakSelf addFiles:files]; };
    self.window.contentView = root;
    self.notice = [NSTextField wrappingLabelWithString:@""];
    self.notice.textColor = NSColor.secondaryLabelColor;
    self.notice.maximumNumberOfLines = 3;
    self.title = [NSTextField labelWithString:self.setup ? @"Connect Guest Chat" : @"Guest chat"];
    self.title.font = [NSFont systemFontOfSize:24 weight:NSFontWeightSemibold];
    self.title.accessibilityTitle = self.title.stringValue;
    NSMutableArray<NSView *> *views = [NSMutableArray arrayWithObject:self.title];
    NSView *expanding;
    NSView *focus;
    NSScrollView *scrollableTranscript = nil;
    if (self.setup) {
        NSTextField *instructions = [NSTextField wrappingLabelWithString:
            @"Choose your service account JSON file and paste the connection details you received."];
        self.chooseKey = Button(@"Choose credential file…", self, @selector(pickFiles:));
        NSScrollView *detailsScroll = [NSTextView scrollablePlainDocumentContentTextView];
        self.details = (NSTextView *)detailsScroll.documentView;
        self.details.font = [NSFont monospacedSystemFontOfSize:13 weight:NSFontWeightRegular];
        self.details.accessibilityLabel = @"Connection details";
        self.details.automaticQuoteSubstitutionEnabled = NO;
        self.details.automaticDashSubstitutionEnabled = NO;
        self.details.automaticTextReplacementEnabled = NO;
        self.connectButton = Button(@"Connect", self, @selector(connect:));
        self.connectButton.bezelStyle = NSBezelStyleRounded;
        self.connectButton.keyEquivalent = @"\r";
        self.connectButton.keyEquivalentModifierMask = NSEventModifierFlagCommand;
        [views addObjectsFromArray:@[instructions, self.chooseKey, detailsScroll, self.notice, self.connectButton]];
        expanding = detailsScroll;
        focus = self.details;
    } else {
        self.presence = [NSTextField labelWithString:@"Offline"];
        self.connection = [NSTextField labelWithString:@"Connecting"];
        self.presence.textColor = self.connection.textColor = NSColor.secondaryLabelColor;
        NSStackView *status = Stack(@[self.presence, Spacer(), self.connection],
            NSUserInterfaceLayoutOrientationHorizontal);
        NSScrollView *transcript = [NSTextView scrollableTextView];
        transcript.borderType = NSBezelBorder;
        self.history = (NSTextView *)transcript.documentView;
        // HTML-imported tables and image attachment cells use TextKit 1.
        (void)self.history.layoutManager;
        [self.history.textContainer replaceLayoutManager:[GuestHistoryLayout new]];
        self.history.textContainerInset = NSMakeSize(12, 10);
        self.history.editable = NO;
        self.history.selectable = YES;
        self.history.richText = YES;
        self.history.delegate = self;
        self.history.accessibilityLabel = @"Conversation";
        self.history.linkTextAttributes = @{NSForegroundColorAttributeName: NSColor.linkColor,
            NSUnderlineStyleAttributeName: @(NSUnderlineStyleSingle)};
        self.draftScroll = [NSScrollView new];
        self.draftScroll.hasVerticalScroller = YES;
        self.draftScroll.hidden = YES;
        self.draftHeight = [self.draftScroll.heightAnchor constraintEqualToConstant:0];
        self.draftHeight.active = YES;
        self.drafts = Stack(@[], NSUserInterfaceLayoutOrientationVertical);
        self.drafts.spacing = 2;
        self.drafts.translatesAutoresizingMaskIntoConstraints = NO;
        self.draftScroll.documentView = self.drafts;

        NSScrollView *composeScroll = [NSScrollView new];
        composeScroll.borderType = NSBezelBorder;
        composeScroll.hasVerticalScroller = YES;
        self.composer = [[GuestComposer alloc] initWithFrame:NSMakeRect(0, 0, 450, 78)];
        self.composer.minSize = NSMakeSize(0, 78);
        self.composer.maxSize = NSMakeSize(CGFLOAT_MAX, CGFLOAT_MAX);
        self.composer.verticallyResizable = YES;
        self.composer.horizontallyResizable = NO;
        self.composer.autoresizingMask = NSViewWidthSizable;
        self.composer.textContainer.containerSize = NSMakeSize(450, CGFLOAT_MAX);
        self.composer.textContainer.widthTracksTextView = YES;
        self.composer.textContainerInset = NSMakeSize(8, 6);
        self.composer.font = [NSFont systemFontOfSize:14];
        self.composer.richText = NO;
        self.composer.allowsUndo = YES;
        self.composer.importsGraphics = NO;
        self.composer.accessibilityLabel = @"Write a message";
        self.composer.sendMessage = ^{ [weakSelf send:nil]; };
        composeScroll.documentView = self.composer;
        [composeScroll.heightAnchor constraintEqualToConstant:82].active = YES;

        NSTextField *lineHint = [NSTextField labelWithString:@"Shift–Return for a new line"];
        lineHint.textColor = NSColor.secondaryLabelColor;
        NSStackView *actions = Stack(@[Button(@"Attach files…", self, @selector(pickFiles:)),
            lineHint, Spacer(), Button(@"Send", self, @selector(send:))],
            NSUserInterfaceLayoutOrientationHorizontal);
        [views addObjectsFromArray:@[status, transcript, self.draftScroll, self.notice, composeScroll, actions]];
        scrollableTranscript = transcript;
        expanding = transcript;
        focus = self.composer;
    }
    NSStackView *layout = Stack(views, NSUserInterfaceLayoutOrientationVertical);
    layout.translatesAutoresizingMaskIntoConstraints = NO;
    [root addSubview:layout];
    [NSLayoutConstraint activateConstraints:@[
        [layout.topAnchor constraintEqualToAnchor:root.topAnchor constant:16],
        [layout.leadingAnchor constraintEqualToAnchor:root.leadingAnchor constant:18],
        [layout.trailingAnchor constraintEqualToAnchor:root.trailingAnchor constant:-18],
        [layout.bottomAnchor constraintEqualToAnchor:root.bottomAnchor constant:-14],
        [expanding.heightAnchor constraintGreaterThanOrEqualToConstant:100],
    ]];
    for (NSView *view in views) {
        if ([view isKindOfClass:NSScrollView.class] || [view isKindOfClass:NSStackView.class] ||
            [view isKindOfClass:NSTextField.class]) {
            [view.widthAnchor constraintEqualToAnchor:layout.widthAnchor].active = YES;
        }
    }
    if (scrollableTranscript) {
        [scrollableTranscript setContentHuggingPriority:NSLayoutPriorityDefaultLow
            forOrientation:NSLayoutConstraintOrientationVertical];
    }
    [self.notice setContentHuggingPriority:NSLayoutPriorityDefaultHigh forOrientation:NSLayoutConstraintOrientationVertical];
    [self.window makeFirstResponder:(NSResponder *)focus];
}

- (void)refresh {
    // The native HTML importer can run AppKit's event loop while importing.
    if (self.renderingHistory) return;
    [self updateBubbleLayout];
    NSDictionary *snapshot = [self request:@{@"operation": @"poll"}];
    if (!snapshot) {
        if (self.checking && ![self.notice.stringValue isEqualToString:@"Checking connection…"]) {
            [self setChecking:NO];
        }
        return;
    }
    if (self.setup) {
        self.setup = NO;
        self.checking = NO;
        [self buildWindow];
    }
    NSString *resident = String(snapshot[@"resident"]);
    if (resident.length) self.title.stringValue = resident;
    self.presence.stringValue = [String(snapshot[@"presence"]) capitalizedString];
    NSUInteger pending = [snapshot[@"pending"] unsignedIntegerValue];
    NSString *status = String(snapshot[@"status"]);
    self.connection.stringValue = pending
        ? [NSString stringWithFormat:@"%@ · %lu queued", status, (unsigned long)pending] : status;
    NSArray *entries = snapshot[@"entries"];
    if (!entries.count) return;
    NSScrollView *scroll = self.history.enclosingScrollView;
    BOOL atBottom = NSMaxY(scroll.documentVisibleRect) >= NSMaxY(self.history.bounds) - 24;
    NSArray *selection = self.history.selectedRanges;
    NSMutableAttributedString *added = [NSMutableAttributedString new];
    self.renderingHistory = YES;
    for (NSDictionary *entry in entries) {
        [added appendAttributedString:[self renderEntry:entry resident:resident]];
        self.messageCount++;
    }
    self.renderingHistory = NO;
    CGFloat width = self.history.textContainer.containerSize.width;
    if (width != self.bubbleLayoutWidth) [self updateBubbleLayout];
    LayoutBubbles(added, NSMakeRange(0, added.length), width);
    [self.history.textStorage appendAttributedString:added];
    self.history.selectedRanges = selection;
    if (atBottom) [self jumpToLatest];
    [self updateBubbleLayout];
}

- (void)windowDidResize:(NSNotification *)notification {
    dispatch_async(dispatch_get_main_queue(), ^{ [self updateBubbleLayout]; });
}

- (void)updateBubbleLayout {
    if (!self.history) return;
    CGFloat width = self.history.textContainer.containerSize.width;
    if (width == self.bubbleLayoutWidth) return;
    self.bubbleLayoutWidth = width;
    NSTextStorage *storage = self.history.textStorage;
    [storage beginEditing];
    LayoutBubbles(storage, NSMakeRange(0, storage.length), width);
    [storage endEditing];
}

- (void)jumpToLatest {
    [self.history scrollRangeToVisible:NSMakeRange(self.history.string.length, 0)];
}

- (NSAttributedString *)renderEntry:(NSDictionary *)entry resident:(NSString *)resident {
    NSString *author = [entry[@"sent"] boolValue] ? @"You" : (resident.length ? resident : @"Resident");
    NSMutableAttributedString *result = [[NSMutableAttributedString alloc]
        initWithString:[author stringByAppendingString:@"\n"] attributes:@{
            NSFontAttributeName: [NSFont systemFontOfSize:12 weight:NSFontWeightSemibold],
            NSForegroundColorAttributeName: NSColor.secondaryLabelColor,
        }];
    NSString *body = String(entry[@"body"]);
    if (body.length) {
        NSString *html = [NSString stringWithFormat:
            @"<!doctype html><meta charset=utf-8><style>body{font:14px -apple-system;margin:0}"
            "pre,code{font-family:Menlo,monospace}table{border-collapse:collapse}"
            "td,th{border:1px solid #999;padding:3px 4px;min-width:5em}</style>%@", String(entry[@"html"])];
        NSData *data = [html dataUsingEncoding:NSUTF8StringEncoding];
        NSMutableAttributedString *rich = [[NSMutableAttributedString alloc] initWithHTML:data
            options:@{NSCharacterEncodingDocumentOption: @(NSUTF8StringEncoding)}
            documentAttributes:nil];
        if (rich.length) {
            // The importer supplies fixed colors. Use the system's adaptive labels.
            [rich addAttribute:NSForegroundColorAttributeName value:NSColor.textColor
                range:NSMakeRange(0, rich.length)];
            [result appendAttributedString:rich];
        } else {
            [result appendAttributedString:[[NSAttributedString alloc] initWithString:body attributes:@{
                NSForegroundColorAttributeName: NSColor.textColor,
                NSFontAttributeName: [NSFont systemFontOfSize:14],
            }]];
        }
        if (![result.string hasSuffix:@"\n"]) {
            [result appendAttributedString:[[NSAttributedString alloc] initWithString:@"\n"]];
        }
    }
    NSUInteger index = 0;
    for (NSDictionary *file in entry[@"files"]) {
        NSString *link = [NSString stringWithFormat:@"boxen-attachment:%lu/%lu",
            (unsigned long)self.messageCount, (unsigned long)index++];
        NSImage *image = Image(file[@"thumbnail"]);
        if (image) {
            NSTextAttachment *attachment = [NSTextAttachment new];
            attachment.attachmentCell = [[NSTextAttachmentCell alloc] initImageCell:image];
            NSMutableAttributedString *thumbnail = [[NSAttributedString attributedStringWithAttachment:attachment] mutableCopy];
            [thumbnail addAttribute:NSLinkAttributeName value:link range:NSMakeRange(0, thumbnail.length)];
            [result appendAttributedString:thumbnail];
            [result appendAttributedString:[[NSAttributedString alloc] initWithString:@"\n"]];
        }
        NSString *size = [NSByteCountFormatter stringFromByteCount:[file[@"size"] longLongValue]
            countStyle:NSByteCountFormatterCountStyleFile];
        NSString *label = [NSString stringWithFormat:@"%@ (%@)\n", String(file[@"name"]), size];
        [result appendAttributedString:[[NSAttributedString alloc] initWithString:label attributes:@{
            NSLinkAttributeName: link, NSFontAttributeName: [NSFont systemFontOfSize:13],
        }]];
    }
    for (NSString *name in entry[@"unavailable"]) {
        [result appendAttributedString:[[NSAttributedString alloc]
            initWithString:[name stringByAppendingString:@" · unavailable\n"] attributes:@{
                NSForegroundColorAttributeName: NSColor.secondaryLabelColor,
                NSFontAttributeName: [NSFont systemFontOfSize:13],
            }]];
    }
    GuestBubble *bubble = [GuestBubble new];
    bubble.sent = [entry[@"sent"] boolValue];
    bubble.preferredWidth = ceil([result boundingRectWithSize:NSMakeSize(700, CGFLOAT_MAX)
        options:NSStringDrawingUsesLineFragmentOrigin].size.width);
    NSRange message = NSMakeRange(0, result.length);
    [result enumerateAttribute:NSParagraphStyleAttributeName inRange:message options:0
        usingBlock:^(NSParagraphStyle *style, NSRange range, BOOL * __unused stop) {
        [result addAttribute:OriginalParagraph value:style ?: NSParagraphStyle.defaultParagraphStyle range:range];
    }];
    [result addAttribute:MessageBubble value:bubble range:message];
    [result appendAttributedString:[[NSAttributedString alloc] initWithString:@"\n"]];
    return result;
}

- (BOOL)textView:(NSTextView *)view clickedOnLink:(id)link atIndex:(NSUInteger)index {
    NSString *url = [link isKindOfClass:NSURL.class] ? [link absoluteString] : String(link);
    if ([url hasPrefix:@"boxen-attachment:"]) {
        NSArray *parts = [[url substringFromIndex:@"boxen-attachment:".length] componentsSeparatedByString:@"/"];
        if (parts.count == 2) [self openMessage:@([parts[0] longLongValue]) file:@([parts[1] longLongValue])];
    } else if ([url hasPrefix:@"https://"] || [url hasPrefix:@"http://"]) {
        [NSPasteboard.generalPasteboard clearContents];
        BOOL copied = [NSPasteboard.generalPasteboard setString:url forType:NSPasteboardTypeString];
        self.notice.stringValue = copied ? @"Link copied." : @"Could not copy the link.";
    }
    return YES;
}

- (void)openMessage:(NSNumber *)message file:(NSNumber *)file {
    NSDictionary *preview = [self request:@{@"operation": @"preview", @"message": message, @"index": file}];
    if (!preview) return;
    void (^save)(void) = ^{
        NSSavePanel *panel = [NSSavePanel savePanel];
        panel.nameFieldStringValue = String(preview[@"name"]);
        [panel beginSheetModalForWindow:self.window completionHandler:^(NSModalResponse response) {
            if (response == NSModalResponseOK) {
                self.notice.stringValue = @"";
                [self request:@{@"operation": @"save", @"message": message, @"index": file, @"path": panel.URL.path}];
            }
        }];
    };
    NSImage *image = Image(preview[@"image"]);
    if (image) {
        [self showImage:image name:String(preview[@"name"]) save:save];
    } else {
        save();
    }
}

- (void)showImage:(NSImage *)image name:(NSString *)name save:(void (^)(void))save {
    CGFloat scale = MIN(1, MIN(620 / image.size.width, 450 / image.size.height));
    NSImageView *view = [NSImageView imageViewWithImage:image];
    view.frame = NSMakeRect(0, 0, image.size.width * scale, image.size.height * scale);
    view.imageScaling = NSImageScaleProportionallyUpOrDown;
    NSAlert *alert = [NSAlert new];
    alert.messageText = name;
    alert.accessoryView = view;
    [alert addButtonWithTitle:@"Close"];
    if (save) [alert addButtonWithTitle:@"Save As…"];
    [alert beginSheetModalForWindow:self.window completionHandler:^(NSModalResponse response) {
        if (save && response == NSAlertSecondButtonReturn) save();
    }];
}

- (void)pickFiles:(id)sender {
    NSOpenPanel *panel = [NSOpenPanel openPanel];
    panel.allowsMultipleSelection = !self.setup;
    [panel beginSheetModalForWindow:self.window completionHandler:^(NSModalResponse response) {
        if (response == NSModalResponseOK) [self addFiles:panel.URLs];
    }];
}

- (BOOL)addFiles:(NSArray<NSURL *> *)files {
    if (self.setup) {
        if (self.checking) return NO;
        self.key = files.firstObject;
        self.chooseKey.title = self.key.lastPathComponent;
        self.notice.stringValue = @"";
    } else {
        NSArray *paths = [files valueForKey:@"path"];
        [self updateDrafts:[self request:@{@"operation": @"attach", @"paths": paths}]];
    }
    return YES;
}

- (void)updateDrafts:(NSDictionary *)draft {
    if (!draft) return;
    for (NSView *view in self.drafts.arrangedSubviews) {
        [self.drafts removeArrangedSubview:view];
        [view removeFromSuperview];
    }
    NSUInteger index = 0;
    for (NSDictionary *file in draft[@"files"]) {
        NSString *name = String(file[@"name"]);
        NSButton *preview = Button(name, self, @selector(previewDraft:));
        preview.tag = index;
        NSImage *thumbnail = Image(file[@"thumbnail"]);
        if (thumbnail) {
            preview.image = thumbnail;
            preview.imagePosition = NSImageLeft;
        } else {
            preview.enabled = NO;
        }
        NSButton *remove = Button(@"Remove", self, @selector(removeDraft:));
        remove.accessibilityLabel = [@"Remove " stringByAppendingString:name];
        remove.tag = index++;
        [self.drafts addArrangedSubview:Stack(@[preview, remove], NSUserInterfaceLayoutOrientationHorizontal)];
    }
    self.draftScroll.hidden = index == 0;
    NSSize size = self.drafts.fittingSize;
    self.drafts.frame = NSMakeRect(0, 0, size.width, size.height);
    self.draftHeight.constant = MIN(85, size.height);
    [self.draftScroll.contentView scrollToPoint:NSMakePoint(0, self.drafts.isFlipped ? 0 : MAX(0, size.height - 85))];
    self.notice.stringValue = String(draft[@"notice"]);
}

- (void)removeDraft:(NSButton *)button {
    [self updateDrafts:[self request:@{@"operation": @"remove", @"index": @(button.tag)}]];
}

- (void)previewDraft:(NSButton *)button {
    NSDictionary *preview = [self request:@{@"operation": @"preview_draft", @"index": @(button.tag)}];
    NSImage *image = Image(preview[@"image"]);
    if (image) [self showImage:image name:String(preview[@"name"]) save:nil];
}

- (void)send:(id)sender {
    self.notice.stringValue = @"";
    NSNumber *accepted = [self request:@{@"operation": @"send", @"body": self.composer.string}];
    if (accepted.boolValue) {
        self.composer.string = @"";
        [self.composer.undoManager removeAllActions];
        [self updateDrafts:@{@"files": @[]}];
        [self refresh];
        [self jumpToLatest];
    }
}

- (void)setChecking:(BOOL)checking {
    _checking = checking;
    self.details.editable = !checking;
    self.chooseKey.enabled = self.connectButton.enabled = !checking;
}

- (void)connect:(id)sender {
    if (!self.key) {
        self.notice.stringValue = @"Choose your service account JSON file.";
        return;
    }
    self.notice.stringValue = @"Checking connection…";
    [self setChecking:YES];
    [self request:@{@"operation": @"connect", @"details": self.details.string, @"path": self.key.path}];
}
@end

static void SetMenu(void) {
    NSMenu *bar = [NSMenu new];
    NSMenuItem *app = [NSMenuItem new];
    NSMenu *appMenu = [[NSMenu alloc] initWithTitle:@"Boxen Guest Chat"];
    [appMenu addItemWithTitle:@"Quit Boxen Guest Chat" action:@selector(terminate:) keyEquivalent:@"q"];
    app.submenu = appMenu;
    [bar addItem:app];
    NSMenuItem *edit = [NSMenuItem new];
    NSMenu *editMenu = [[NSMenu alloc] initWithTitle:@"Edit"];
    [editMenu addItemWithTitle:@"Undo" action:@selector(undo:) keyEquivalent:@"z"];
    [editMenu addItemWithTitle:@"Redo" action:@selector(redo:) keyEquivalent:@"Z"];
    [editMenu addItem:NSMenuItem.separatorItem];
    [editMenu addItemWithTitle:@"Cut" action:@selector(cut:) keyEquivalent:@"x"];
    [editMenu addItemWithTitle:@"Copy" action:@selector(copy:) keyEquivalent:@"c"];
    [editMenu addItemWithTitle:@"Paste" action:@selector(paste:) keyEquivalent:@"v"];
    [editMenu addItemWithTitle:@"Select All" action:@selector(selectAll:) keyEquivalent:@"a"];
    edit.submenu = editMenu;
    [bar addItem:edit];
    NSApp.mainMenu = bar;
}

void guest_chat_run(void *state, bool setup) {
    @autoreleasepool {
        [NSApplication sharedApplication];
        [NSApp setActivationPolicy:NSApplicationActivationPolicyRegular];
        SetMenu();
        __attribute__((objc_precise_lifetime)) GuestChat *chat = [GuestChat new];
        chat.state = state;
        chat.setup = setup;
        NSApp.delegate = chat;
        [NSApp run];
    }
}
