// Objective-C sample
#import <Foundation/Foundation.h>
@interface Greeter : NSObject
- (NSString *)greet:(NSString *)name;
@end
@implementation Greeter
// Greets.
- (NSString *)greet:(NSString *)name {
    return [NSString stringWithFormat:@"hi %@", name];
}
+ (instancetype)make { return [[Greeter alloc] init]; }
@end
static int helper(int x) { return x * 2; }
