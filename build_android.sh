cargo ndk -t aarch64-linux-android -o ./launcher/mobile/android/app/src/main/jniLibs build -p mobile

pushd launcher/mobile/android
./gradlew build
popd