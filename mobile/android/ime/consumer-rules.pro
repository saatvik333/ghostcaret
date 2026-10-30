# The engine looks these up by name over JNI.
-keep class io.github.saatvik333.ghostcaret.ime.Bridge { public <methods>; }
-keepclasseswithmembernames,includedescriptorclasses class io.github.saatvik333.ghostcaret.ime.Native {
    native <methods>;
}
