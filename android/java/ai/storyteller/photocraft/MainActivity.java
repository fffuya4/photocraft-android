package ai.storyteller.photocraft;

import android.app.NativeActivity;
import android.content.Intent;
import android.os.Bundle;

/** The native activity, plus the one thing it must do in Java: receive the file picker's answers. */
public final class MainActivity extends NativeActivity {
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        FileBridge.activity = this;
        super.onCreate(savedInstanceState);
    }

    @Override
    protected void onDestroy() {
        if (FileBridge.activity == this) {
            FileBridge.activity = null;
        }
        super.onDestroy();
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        FileBridge.onResult(this, requestCode, resultCode, data);
        super.onActivityResult(requestCode, resultCode, data);
    }
}
