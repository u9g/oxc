//! <https://github.com/google/closure-compiler/blob/master/test/com/google/javascript/jscomp/PeepholeMinimizeConditionsTest.java>

use crate::test;

#[test]
fn test_returns() {
    test("function f(){if(x)return 1;else return 2}", "function f(){return x?1:2}");
    // test("function f(){if(x)return 1;return 2}", "function f(){return x?1:2}");
    // test("function f(){if(x)return;return 2}", "function f(){return x?void 0:2}");
    // test("function f(){if(x)return 1+x;else return 2-x}", "function f(){return x?1+x:2-x}");
    // test("function f(){if(x)return 1+x;return 2-x}", "function f(){return x?1+x:2-x}");
    // test(
    // "function f(){if(x)return y += 1;else return y += 2}",
    // "function f(){return x?(y+=1):(y+=2)}",
    // );

    // test("function f(){if(x)return;else return 2-x}", "function f(){if(x);else return 2-x}");
    // test("function f(){if(x)return;return 2-x}", "function f(){return x?void 0:2-x}");
    // test("function f(){if(x)return x;else return}", "function f(){if(x)return x;{}}");
    // test("function f(){if(x)return x;return}", "function f(){if(x)return x}");

    // test_same("function f(){for(var x in y) { return x.y; } return k}");
}
