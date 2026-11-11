package org.resumo.app;

import android.app.Activity;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.media.MediaMetadata;
import android.media.session.MediaSession;
import android.media.session.PlaybackState;
import android.os.Build;
import android.os.Handler;
import android.os.Looper;

/**
 * The player as Android sees it from outside the app: a media session (what
 * the lock screen, the notification shade, headphones and watches control)
 * and the media notification of {@link PlaybackService}, which keeps the
 * process playing with the screen off. The Rust side calls the static
 * methods; button presses come back through {@link #action}.
 */
public final class Playback {
    public static final int PLAY = 1, PAUSE = 2, NEXT = 3, PREV = 4, SEEK = 5, STOP = 6, TOGGLE = 7;

    static final String CHANNEL = "playback";
    static final int NOTIFICATION_ID = 1;

    /** Handled by the Rust side (registered from native code). */
    static native void action(int code, long arg);

    private static Context app;
    private static Class<?> activityClass;
    private static MediaSession session;
    private static Bitmap art;
    private static String title = "", subtitle = "";
    private static boolean playing, active;
    private static Handler main;

    /** Once, with the activity; `icon` is the app's PNG icon. */
    public static void init(final Activity activity, final byte[] icon) {
        app = activity.getApplicationContext();
        activityClass = activity.getClass();
        main = new Handler(Looper.getMainLooper());
        main.post(new Runnable() {
            public void run() {
                if (icon != null) art = BitmapFactory.decodeByteArray(icon, 0, icon.length);
                if (Build.VERSION.SDK_INT >= 26) {
                    NotificationChannel ch = new NotificationChannel(CHANNEL, "Воспроизведение", NotificationManager.IMPORTANCE_LOW);
                    ch.setShowBadge(false);
                    ch.setLockscreenVisibility(Notification.VISIBILITY_PUBLIC);
                    ((NotificationManager) app.getSystemService(Context.NOTIFICATION_SERVICE)).createNotificationChannel(ch);
                }
                session = new MediaSession(app, "Resumo");
                session.setCallback(new MediaSession.Callback() {
                    @Override public void onPlay() { action(PLAY, 0); }
                    @Override public void onPause() { action(PAUSE, 0); }
                    @Override public void onSkipToNext() { action(NEXT, 0); }
                    @Override public void onSkipToPrevious() { action(PREV, 0); }
                    @Override public void onSeekTo(long pos) { action(SEEK, pos); }
                    @Override public void onStop() { action(STOP, 0); }
                    @Override public void onFastForward() { action(SEEK, -10000 - 1); }
                    @Override public void onRewind() { action(SEEK, -10000 - 2); }
                });
                Intent open = new Intent(app, activityClass).setFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP | Intent.FLAG_ACTIVITY_REORDER_TO_FRONT);
                session.setSessionActivity(PendingIntent.getActivity(app, 0, open, PendingIntent.FLAG_IMMUTABLE));
            }
        });
    }

    /**
     * The state to show. `active` false (nothing loaded) takes the session
     * and the notification away. Position and length in ms (length -1 if
     * unknown).
     */
    public static void update(final boolean isActive, final String t, final String sub, final boolean isPlaying,
                              final long position, final long length, final float rate) {
        if (main == null) return;
        main.post(new Runnable() {
            public void run() {
                if (session == null) return;
                if (!isActive) {
                    if (active) {
                        active = false;
                        playing = false;
                        session.setActive(false);
                        app.stopService(new Intent(app, PlaybackService.class));
                    }
                    return;
                }
                boolean changed = !t.equals(title) || !sub.equals(subtitle) || isPlaying != playing || !active;
                title = t;
                subtitle = sub;
                playing = isPlaying;
                MediaMetadata.Builder meta = new MediaMetadata.Builder()
                        .putString(MediaMetadata.METADATA_KEY_TITLE, t)
                        .putString(MediaMetadata.METADATA_KEY_DISPLAY_TITLE, t)
                        .putString(MediaMetadata.METADATA_KEY_ARTIST, sub)
                        .putString(MediaMetadata.METADATA_KEY_DISPLAY_SUBTITLE, sub)
                        .putLong(MediaMetadata.METADATA_KEY_DURATION, length);
                if (art != null) meta.putBitmap(MediaMetadata.METADATA_KEY_ART, art);
                session.setMetadata(meta.build());
                long actions = PlaybackState.ACTION_PLAY | PlaybackState.ACTION_PAUSE | PlaybackState.ACTION_PLAY_PAUSE
                        | PlaybackState.ACTION_SKIP_TO_NEXT | PlaybackState.ACTION_SKIP_TO_PREVIOUS | PlaybackState.ACTION_STOP
                        | PlaybackState.ACTION_FAST_FORWARD | PlaybackState.ACTION_REWIND;
                if (length > 0) actions |= PlaybackState.ACTION_SEEK_TO;
                session.setPlaybackState(new PlaybackState.Builder()
                        .setActions(actions)
                        .setState(isPlaying ? PlaybackState.STATE_PLAYING : PlaybackState.STATE_PAUSED, position,
                                isPlaying ? rate : 0f)
                        .build());
                if (!active) {
                    active = true;
                    session.setActive(true);
                }
                if (changed) {
                    Intent svc = new Intent(app, PlaybackService.class);
                    try {
                        if (Build.VERSION.SDK_INT >= 26) app.startForegroundService(svc);
                        else app.startService(svc);
                    } catch (RuntimeException e) {
                        // Not allowed from the background (Android 12+): the
                        // notification is updated in place instead.
                        PlaybackService.refresh(app);
                    }
                }
            }
        });
    }

    static boolean isPlaying() {
        return playing;
    }

    static Notification notification(Context ctx) {
        Notification.Builder b = Build.VERSION.SDK_INT >= 26 ? new Notification.Builder(ctx, CHANNEL) : new Notification.Builder(ctx);
        b.setSmallIcon(android.R.drawable.ic_media_play)
                .setContentTitle(title)
                .setContentText(subtitle)
                .setVisibility(Notification.VISIBILITY_PUBLIC)
                .setOngoing(playing)
                .setShowWhen(false)
                .addAction(button(ctx, android.R.drawable.ic_media_previous, "Назад", PREV))
                .addAction(playing ? button(ctx, android.R.drawable.ic_media_pause, "Пауза", PAUSE)
                                   : button(ctx, android.R.drawable.ic_media_play, "Играть", PLAY))
                .addAction(button(ctx, android.R.drawable.ic_media_next, "Вперёд", NEXT))
                .setDeleteIntent(PlaybackService.intent(ctx, STOP));
        if (art != null) b.setLargeIcon(art);
        if (session != null) {
            b.setStyle(new Notification.MediaStyle().setMediaSession(session.getSessionToken()).setShowActionsInCompactView(0, 1, 2));
            b.setContentIntent(session.getController().getSessionActivity());
        }
        if (Build.VERSION.SDK_INT >= 31) b.setForegroundServiceBehavior(Notification.FOREGROUND_SERVICE_IMMEDIATE);
        return b.build();
    }

    private static Notification.Action button(Context ctx, int icon, String label, int code) {
        return new Notification.Action.Builder(icon, label, PlaybackService.intent(ctx, code)).build();
    }
}
