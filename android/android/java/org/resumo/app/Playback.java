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
import android.media.AudioAttributes;
import android.media.AudioFocusRequest;
import android.media.AudioManager;
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

    /**
     * The notification channel. A channel's importance can't be changed once
     * created, so raising it took a new id; the old one is deleted.
     */
    static final String CHANNEL = "playback_controls";
    private static final String OLD_CHANNEL = "playback";
    static final int NOTIFICATION_ID = 1;

    /** Handled by the Rust side (registered from native code). */
    static native void action(int code, long arg);

    private static Context app;
    private static Class<?> activityClass;
    private static MediaSession session;
    private static Bitmap art;
    private static String title = "", subtitle = "";
    private static boolean playing, active;
    private static long positionMs, lengthMs = -1;
    /** Position in steps of PROGRESS_STEP_MS, as last shown in the notification. */
    private static long shownStep = -1;
    private static final long PROGRESS_STEP_MS = 5000;
    private static Handler main;
    private static AudioManager audio;
    private static AudioFocusRequest focusRequest;
    private static boolean hasFocus;
    private static final AudioAttributes ATTRS = new AudioAttributes.Builder()
            .setUsage(AudioAttributes.USAGE_MEDIA)
            .setContentType(AudioAttributes.CONTENT_TYPE_MOVIE)
            .build();

    /**
     * Another app took the sound (a call, music, a video): pause, like other
     * players. Focus is asked for again on the next play.
     */
    private static final AudioManager.OnAudioFocusChangeListener FOCUS = new AudioManager.OnAudioFocusChangeListener() {
        public void onAudioFocusChange(int change) {
            if (change == AudioManager.AUDIOFOCUS_LOSS || change == AudioManager.AUDIOFOCUS_LOSS_TRANSIENT) {
                hasFocus = false;
                if (playing) action(PAUSE, 0);
            }
        }
    };

    /** Once, with the activity; `icon` is the app's PNG icon. */
    public static void init(final Activity activity, final byte[] icon) {
        app = activity.getApplicationContext();
        activityClass = activity.getClass();
        main = new Handler(Looper.getMainLooper());
        main.post(new Runnable() {
            public void run() {
                if (icon != null) art = BitmapFactory.decodeByteArray(icon, 0, icon.length);
                if (Build.VERSION.SDK_INT >= 26) {
                    // Default importance, but silent: some skins (Vivo's
                    // OriginOS / Funtouch among them) keep "low" (silent)
                    // notifications, media ones included, off the lock screen.
                    NotificationChannel ch = new NotificationChannel(CHANNEL, "Управление воспроизведением", NotificationManager.IMPORTANCE_DEFAULT);
                    ch.setSound(null, null);
                    ch.enableVibration(false);
                    ch.enableLights(false);
                    ch.setShowBadge(false);
                    ch.setLockscreenVisibility(Notification.VISIBILITY_PUBLIC);
                    NotificationManager nm = (NotificationManager) app.getSystemService(Context.NOTIFICATION_SERVICE);
                    nm.createNotificationChannel(ch);
                    nm.deleteNotificationChannel(OLD_CHANNEL);
                }
                audio = (AudioManager) app.getSystemService(Context.AUDIO_SERVICE);
                if (Build.VERSION.SDK_INT >= 26) {
                    focusRequest = new AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
                            .setAudioAttributes(ATTRS)
                            .setOnAudioFocusChangeListener(FOCUS, main)
                            .build();
                }
                session = new MediaSession(app, "Resumo");
                // The lock screen shows the session of the app that plays and
                // holds the audio focus (like Spotify); say it is local media.
                session.setPlaybackToLocal(ATTRS);
                // Implied from Android 8 on; Android 7 (and lock screens that
                // still look at them) need the flags to offer controls.
                session.setFlags(MediaSession.FLAG_HANDLES_MEDIA_BUTTONS | MediaSession.FLAG_HANDLES_TRANSPORT_CONTROLS);
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
                        abandonFocus();
                        app.stopService(new Intent(app, PlaybackService.class));
                    }
                    return;
                }
                boolean changed = !t.equals(title) || !sub.equals(subtitle) || isPlaying != playing || !active;
                title = t;
                subtitle = sub;
                playing = isPlaying;
                positionMs = position;
                lengthMs = length;
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
                if (isPlaying && !hasFocus) requestFocus();
                if (!active) {
                    active = true;
                    session.setActive(true);
                }
                long step = length > 0 ? position / PROGRESS_STEP_MS : -1;
                if (!changed && step != shownStep) {
                    // Only the progress bar moved: no need to poke the service.
                    shownStep = step;
                    PlaybackService.refresh(app);
                }
                if (changed) {
                    shownStep = step;
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

    private static void requestFocus() {
        if (audio == null) return;
        int r = Build.VERSION.SDK_INT >= 26 ? audio.requestAudioFocus(focusRequest)
                : audio.requestAudioFocus(FOCUS, AudioManager.STREAM_MUSIC, AudioManager.AUDIOFOCUS_GAIN);
        hasFocus = r == AudioManager.AUDIOFOCUS_REQUEST_GRANTED;
    }

    private static void abandonFocus() {
        if (audio == null || !hasFocus) return;
        if (Build.VERSION.SDK_INT >= 26) audio.abandonAudioFocusRequest(focusRequest);
        else audio.abandonAudioFocus(FOCUS);
        hasFocus = false;
    }

    static boolean isPlaying() {
        return playing;
    }

    static Notification notification(Context ctx) {
        Notification.Builder b = Build.VERSION.SDK_INT >= 26 ? new Notification.Builder(ctx, CHANNEL) : new Notification.Builder(ctx);
        b.setSmallIcon(icon(ctx, "ic_np_play", android.R.drawable.ic_media_play))
                .setContentTitle(title)
                .setContentText(subtitle)
                .setVisibility(Notification.VISIBILITY_PUBLIC)
                .setCategory(Notification.CATEGORY_TRANSPORT)
                .setPriority(Notification.PRIORITY_DEFAULT)
                .setOnlyAlertOnce(true)
                .setOngoing(playing)
                .setShowWhen(false)
                .addAction(button(ctx, icon(ctx, "ic_np_prev", android.R.drawable.ic_media_previous), "Назад", PREV))
                .addAction(playing ? button(ctx, icon(ctx, "ic_np_pause", android.R.drawable.ic_media_pause), "Пауза", PAUSE)
                                   : button(ctx, icon(ctx, "ic_np_play", android.R.drawable.ic_media_play), "Играть", PLAY))
                .addAction(button(ctx, icon(ctx, "ic_np_next", android.R.drawable.ic_media_next), "Вперёд", NEXT))
                .setDeleteIntent(PlaybackService.intent(ctx, STOP));
        // Stock Android draws the bar from the session; skins with their own
        // media layout (Vivo) may only know the notification's own progress.
        if (lengthMs > 0) b.setProgress(1000, (int) Math.max(0, Math.min(1000, positionMs * 1000 / lengthMs)), false);
        if (art != null) b.setLargeIcon(art);
        if (session != null) {
            b.setStyle(new Notification.MediaStyle().setMediaSession(session.getSessionToken()).setShowActionsInCompactView(0, 1, 2));
            b.setContentIntent(session.getController().getSessionActivity());
        }
        if (Build.VERSION.SDK_INT >= 31) b.setForegroundServiceBehavior(Notification.FOREGROUND_SERVICE_IMMEDIATE);
        return b.build();
    }

    private static Notification.Action button(Context ctx, int icon, String label, int code) {
        return new Notification.Action.Builder(android.graphics.drawable.Icon.createWithResource(ctx, icon), label,
                PlaybackService.intent(ctx, code)).build();
    }

    /**
     * The app's own sharp vector icon (res/drawable), by name: the build has
     * no generated R class. The system's old bitmap icons look blurred on
     * today's screens, so they are only the fallback.
     */
    private static int icon(Context ctx, String name, int fallback) {
        int id = ctx.getResources().getIdentifier(name, "drawable", ctx.getPackageName());
        return id != 0 ? id : fallback;
    }
}
