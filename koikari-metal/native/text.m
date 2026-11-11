#import <AppKit/AppKit.h>
#import <CoreGraphics/CoreGraphics.h>
#include <stdint.h>
#include <stdlib.h>

// Tiny platform boundary only: the game, archive reader, VM, audio and Metal
// renderer live in Rust. Native text shaping provides CJK fallback and wrapping.
uint8_t *km_text_rgba(const char *utf8, uint32_t width, uint32_t height,
                     float fontSize, float red, float green, float blue,
                     float alpha, int bold, int centered) {
    if (!utf8 || !width || !height || width > 8192 || height > 8192) return NULL;
    @autoreleasepool {
        size_t stride = (size_t)width * 4;
        uint8_t *pixels = calloc((size_t)height, stride);
        if (!pixels) return NULL;
        CGColorSpaceRef space = CGColorSpaceCreateDeviceRGB();
        CGContextRef bitmap = CGBitmapContextCreate(pixels, width, height, 8,
            stride, space, kCGImageAlphaPremultipliedLast | kCGBitmapByteOrder32Big);
        CGColorSpaceRelease(space);
        if (!bitmap) { free(pixels); return NULL; }
        CGContextTranslateCTM(bitmap, 0, height);
        CGContextScaleCTM(bitmap, 1, -1);
        NSGraphicsContext *context = [NSGraphicsContext graphicsContextWithCGContext:bitmap flipped:YES];
        [NSGraphicsContext saveGraphicsState];
        [NSGraphicsContext setCurrentContext:context];
        NSString *text = [[NSString alloc] initWithUTF8String:utf8];
        NSFont *font = [NSFont fontWithName:bold ? @"PingFangSC-Semibold" : @"PingFangSC-Regular" size:fontSize];
        if (!font) font = [NSFont systemFontOfSize:fontSize weight:bold ? NSFontWeightSemibold : NSFontWeightRegular];
        NSMutableParagraphStyle *paragraph = [[NSMutableParagraphStyle alloc] init];
        paragraph.alignment = centered ? NSTextAlignmentCenter : NSTextAlignmentLeft;
        paragraph.lineBreakMode = NSLineBreakByCharWrapping;
        paragraph.lineSpacing = fontSize * 0.20;
        NSDictionary *attributes = @{
            NSFontAttributeName: font,
            NSForegroundColorAttributeName: [NSColor colorWithSRGBRed:red green:green blue:blue alpha:alpha],
            NSParagraphStyleAttributeName: paragraph,
        };
        [text drawWithRect:NSMakeRect(0, 0, width, height)
                  options:NSStringDrawingUsesLineFragmentOrigin | NSStringDrawingUsesFontLeading
               attributes:attributes];
        [NSGraphicsContext restoreGraphicsState];
        CGContextRelease(bitmap);
        return pixels;
    }
}
void km_free_text(uint8_t *pixels) { free(pixels); }
void km_activate(void) {
    @autoreleasepool {
        [NSApp setActivationPolicy:NSApplicationActivationPolicyRegular];
        [NSApp activateIgnoringOtherApps:YES];
    }
}
