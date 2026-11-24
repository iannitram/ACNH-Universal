// Android 14+ (API 34+) user-initiated data transfer job for GameDownload (issue #230).
// Runs without Android 15's 6-hour/day dataSync foreground-service cap and is automatically
// rescheduled by JobScheduler after network loss or device restart.
package io.github.acnhrecomp;

import android.app.NotificationManager;
import android.app.job.JobParameters;
import android.app.job.JobService;

public class GameDownloadJob extends JobService {
    private volatile boolean stopped;

    @Override
    public boolean onStartJob(final JobParameters params) {
        stopped = false;
        GameDownload.ensureChannel(this);
        setNotification(
                params,
                GameDownload.NOTE,
                GameDownload.buildNote(this, "Starting the download", 0, 0),
                JOB_END_NOTIFICATION_POLICY_REMOVE);

        GameDownload.startWorker(this, new GameDownload.ProgressSink() {
            @Override
            public void onProgress(String text, long done, long total) {
                if (!stopped) {
                    setNotification(
                            params,
                            GameDownload.NOTE,
                            GameDownload.buildNote(GameDownloadJob.this, text, done, total),
                            JOB_END_NOTIFICATION_POLICY_REMOVE);
                }
            }

            @Override
            public void onFinish(String fail) {
                if (!stopped) {
                    GameDownload.postDoneNotification(GameDownloadJob.this, fail);
                    jobFinished(params, false);
                }
            }
        });
        return true;
    }

    @Override
    public boolean onStopJob(JobParameters params) {
        stopped = true;
        GameDownload.stopWorker();
        // Reschedule automatically when network / device constraints are satisfied again.
        return true;
    }
}
