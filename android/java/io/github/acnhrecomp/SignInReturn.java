// The way back from the Dropbox sign-in page (acnhrecomp://signed-in, DropboxAuth). Exported for that link only: it hands
// Home the Uri and nothing else, so no extra of the starting intent reaches Home (#364).
package io.github.acnhrecomp;

import android.app.Activity;
import android.content.Intent;
import android.net.Uri;
import android.os.Bundle;

public class SignInReturn extends Activity {
    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        Uri uri = getIntent().getData();
        if (uri != null && "acnhrecomp".equals(uri.getScheme()) && "signed-in".equals(uri.getHost())) {
            startActivity(new Intent(this, Home.class).setData(uri));
        }
        finish();
    }
}
