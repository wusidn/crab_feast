cargo ndk -t aarch64-linux-android -o ./launcher/mobile/android/app/src/main/jniLibs build --release -p crab_feast_mobile

pushd launcher/mobile/android
./gradlew assembleRelease
popd
